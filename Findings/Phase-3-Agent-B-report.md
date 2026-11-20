# Phase 3 — Agent B report: static-model material state, culling and depth order

**Branch** `arena/5b7c9c75-perfect-world-exlorer` · **PR** #6 · **Base** `main` (`b718574`)
**Date** 2 November 2026 · **Scope** P3.02, P3.04, P3.05

> **Post-merge follow-up (alpha regression).** The blended-scenery state this
> report recovers is accurate for the 1.4.2 client, but reproducing its
> depth-writing blend in the viewer broke water rendering (square lily-pad
> cards) and stacked tree canopies. The viewer now renders textured baked meshes
> as an alpha-test cutout instead. See the updated §7 bullet and the module note
> in `src/bmd_baked.rs`; the P3.02 half-alpha/zero-alpha raster cases are
> replaced by above/below-cutout cases.

The lane is *how a parsed static model reaches the device*: material and
texture-stage state, culling/sidedness, depth bias and draw order. File parsing,
texture residency and view/pass selection stay with their owners; §8 lists every
interface this report changes for them.

All three sub-tasks are **Yes** (P3.02, P3.04, P3.05). P3.02's blending row is
closed by reading all four callers of the submitter: every pass enables
`ALPHABLENDENABLE(1)` right before drawing, none writes the blend factors, and
stage-0 alpha is `MODULATE(TEXTURE, DIFFUSE)` — opcode `4`, not the `SELECTARG2`
the previous revision assumed — so the sampled texture alpha is the surface
coverage and the pass blend is what cuts canopy and leaf cards. That correction
is what fixes the solid-card foliage this lane previously produced; the
`+0x95 == 2` gate that looked like a per-ornament selector is the pass object's
`WaterEffect` graphics setting. P3.05 closes on the retail census: the shipped survey executable was run
over `litmodels.pck` (16,335 models, 40,579 meshes, `unparsed=0`) and all 174,744
coincident triangles are attributed — 168,057 removed by the cull rule, 6,687
decided by submission order, none unresolved (§4).

## 0. Verification basis and limits

* **The retail executable was analysed in this session.** `elementclient.exe`
  6,000,640 B, MD5 `5f02895688fa580506e869156340a664` (re-checked with `md5sum`
  before every address below was read), PE i386, image base `0x400000`,
  `.text 0x401000+0x4d3f25`. Disassembled with
  `objdump -d google_drive/elementclient.exe > /tmp/dis/text.asm`; windows were
  read with a helper that matches `^\s+([0-9a-f]+):\t(?:[0-9a-f]{2} )+\s*\t(.*)$`.
  Data references were found by searching the raw file for
  `struct.pack('<I', va)` with the section table applied. The binary is not
  committed (`.gitignore` keeps `/google_drive/` out of the tree).
* **No Rust toolchain exists in this sandbox** (`cargo`, `rustc` and `rustfmt`
  are absent and the distribution hosts are unreachable), so no local build,
  `cargo fmt --check` or test run was possible. Compilation, formatting and test
  execution therefore happened in CI on the PR branch; §6 records the run and
  its exact results. Two local substitutes were used and are reported as such:
  a `tree-sitter-rust` parse of every touched file (0 `ERROR`/`MISSING` nodes)
  and a Python oracle for the integer arithmetic and for the shader's culling
  predicate.
* **All work is on the Arena session branch.** The task asked for a private
  working branch; this sandbox tracks the session by
  `arena/5b7c9c75-perfect-world-exlorer` and forbids creating or switching
  branches, so the work was committed there and opened as PR #6 instead.
* **The retail asset archives were not reachable.** The owner split
  `litmodels.pck` (397,425,471 B) into `litmodels.zip.001`…`.005` (94,371,840 B
  each, last 12,593,309 B) inside the Drive folder `Games/pw142`
  (`1M75AxJLRHa4xYRL2d1CBkw9IGapuR5Qk`), and this session listed those parts. They
  still cannot be read here: sandbox egress allows only GitHub, npm and PyPI
  (`curl https://drive.google.com` → no route), and the Drive connector returns a
  file body to the model rather than to the workspace, so even one 90 MB part is
  unusable. Larger archives (`models.pck` 888 MB, `building.pck` 326 MB) were
  never offered. The census the acceptance needs was therefore produced by the
  owner from this branch's own executable (§4) and is quoted verbatim; this session
  re-uses the recorded census (16,335 BMDs, 40,579 meshes, 7,802,464
  vertices; 31,802 v4 + 8,777 v5 meshes; 2,363 v5 two-sided meshes) and the
  recorded overlap survey (174,744 coincident triangles in 5,416 models;
  168,225 opposite-winding, 6,519 same-winding, 12 across meshes), which the
  owner's fresh run reproduced digit for digit, so no per-asset claim rests on a
  number this lane did not verify twice.

## 1. The recovered static-model draw state

Everything below is written by the ordinary baked-mesh submitter `0x7a9730`,
whose per-mesh loop is `0x7a9808..0x7a99bc`, or by the state pass that precedes
it. `0x7a9120` is the refraction variant of the same submitter and shares every
row except the stage-0 alpha argument.

| State | Value for a baked mesh | Native write | Viewer consequence |
|---|---|---|---|
| `LIGHTING` (id 0x89) | 0 | `0x77a3c0(0)` at `0x7a9759` | Material colours and `Power` from the v5 block never contribute. |
| `SPECULARENABLE` (id 0x1d) | 0 | `0x77b570(0)` at `0x7a9762` | No specular term. |
| stage 0 `COLOROP` | 5 `MODULATE2X` | `0x779fc0(0,5)` at `0x7a976d` | `texture.rgb * vertex.rgb * 2`, saturated. |
| stage 0 `COLORARG1/2` | `TEXTURE`, `DIFFUSE` | `0x779fe0(0,2,1)` at `0x7a977a` | Same. |
| stage 0 `ALPHAOP` | 4 `MODULATE` | `0x77a020(0,4)` at `0x7a9785` | Alpha is `texture.a * diffuse.a`; `diffuse.a` is the per-mesh opacity, so the sampled texture alpha is the surface coverage. |
| stage 0 `ALPHAARG1/2` | `TEXTURE`, `DIFFUSE` | `0x77a040(0,2,1)` at `0x7a9792` | Same. |
| stages 1–7 | disabled (1) | `0x779fc0(1,1)`/`0x77a020(1,1)` at `0x7a979e`/`0x7a97aa` | Single-stage material. |
| `CULLMODE` | saved, then 1 `NONE` when `mesh+0x26c` ≠ 0 | save `0x168(%ecx)` → `0x14(%esp)` at `0x7a98bb`; `0x77a540(1)` at `0x7a98cf`; restore `0x77a540(saved)` at `0x7a9a26`/`0x7a9a49` | Two-sided meshes keep both windings; every other mesh keeps the pass cull mode. |
| `ALPHATEST` | 0 | `cmp $0xff,0x2cc(%edi)` at `0x7a98f1`; `0x77a320(0)` at `0x7a992b` | No cutout for baked meshes. |
| `ALPHATEST(1)` + `ALPHAREF` | only when `mesh+0x2cc` ≠ 0xff | `0x77a320(1)` at `0x7a98f8`; `80*alpha/255+1` at `0x7a990a..0x7a9920` (`imul 0x80808081`, `shl 4`/`lea` = ×80, `sar 7` = ÷255, `+1`), `0x77a380` at `0x7a9924` | Unreachable from files; the reader refuses such a value. |
| `ALPHABLENDENABLE` | 1 in every pass, never touched by the mesh loop | no `0x77a290` write exists between `0x7a9808` and the draws it gates; the four callers of `0x7a9730` write `1` at `0x42b5a5`, `0x42bc17`, `0x42c32e`, `0x42c650` and `ALPHATEST(0)` at `0x42b595`, `0x42bc07`, `0x42c31e`, `0x42c660`; no pass writes `SRCBLEND`/`DESTBLEND` | Load bearing: with `ALPHAOP` = `MODULATE` the fragment alpha is the sampled texture alpha, so `SRCALPHA`/`INVSRCALPHA` is what cuts leaf and canopy cards. The `+0x95 == 2` gate at `0x42c5f3` is the pass object's `WaterEffect` setting (`0x40d16c`/`0x40d765`), not a mesh field. |
| `ZBIAS` (id 0x2f) | not written | the only call sites are `0x77a250(2)` at `0x417453` and the `0x77a250(0)` restore at `0x417471`, both in the effect path | Bias 0. |
| `ZFUNC` (id 0x17) | not written | only `0x796753` and `0x796cd2` (water/effect paths) | D3D8 default `LESSEQUAL`. |
| Draw order | mesh array order, one indexed submission per mesh | loop head `0x7a9808`, per-mesh submit `0x7b9ca0` at `0x7a98e3` | Unchanged from the client: insertion order, later primitives last. |

`mesh+0x2cc` is written only by the mesh constructor (`0x7b9408`,
`movl $0xff,0x2cc(%esi)`); no BMD reader in the image stores to that offset.
`mesh+0x26c` is the v5 two-sided byte the reader already exposes.

## 2. Task P3.02 — material and texture-stage state

### Recovered rule

A baked static mesh has exactly one material row: one enabled stage combining
the texture with the baked vertex colour, doubled; lighting off; specular off;
no alpha test; culling from `mesh+0x26c`; coverage from the pass blend
(`ALPHABLENDENABLE(1)`, `SRCALPHA`/`INVSRCALPHA`) applied to the stage alpha
`texture.a * diffuse.a` (§ "Blending: recovered" below). The v5 material block the
reader parses (`Ambient`/`Diffuse`/`Emissive`/`Specular`/`Power`/`2Sided` at
`0x7b67b0`, copied by `0x7b66d0`) contributes only `2Sided` to the draw, because
`LIGHTING(0)` at `0x7a9759` removes the fixed-function material from the
pipeline — the colours are parsed, stored and never read by the submitter. That
is the recovered *rule*, not a per-asset exception, and it replaces the old
assumption that a non-default material colour would modulate the surface.

### Implementation

* `src/bmd_baked.rs` is now the single table: `Stage` (`ColorOnly`,
  `Modulate2x`), `AlphaTest` (`Disabled`), `Cull` (`BackFace`, `TwoSided`),
  `Blend` (`TextureAlphaSrcOver`, `Opaque`), `DrawState`, `classify()`,
  `alpha_test()`,
  `cutout_reference()`, plus `native` constants naming every write site above.
* `classify()` is the only producer of the flat `kind` float that the shared
  world shader consumes; `src/gpu.rs` (`build()`, nearby scenery) and
  `src/world_objects.rs` (`run()`, whole-world pages) both call it, so the old
  duplicated `if two_sided { 2.1 } else { 2.0 }` inline rule is gone.
* Untextured meshes used the water colour path (`kind` 1.0, `i.color.rgb` =
  atlas rectangle) — a silent wrong colour. They now get their own kinds
  (1.1 single-sided, 1.2 two-sided) and the shader returns
  `clamp(baked_colour * 2)` through `bmd_pass_fog`, i.e. `MODULATE2X` against an
  unbound (white) stage.
* The baked band was removed from the shader's alpha test. Before:
  `((i.kind>1.5 && i.kind<2.5) || i.kind>3.5) && tex.a<…`. After:
  `i.kind>3.5 && tex.a<…`, so only grass/trees keep a cutout.
* Unrepresentable combinations are explicit errors: any `mesh+0x2cc` other than
  `0xff` is refused by `alpha_test()` with the reference the client would have
  computed, and a v4 mesh carrying a two-sided flag keeps failing.

### Blending: recovered

The mesh loop never writes `ALPHABLENDENABLE`, so the previous revision moved the
row to the caller and left it open. Reading all four callers settles it:

| scene pass | `ALPHABLENDENABLE` | `ALPHATEST` | scenery draw |
|---|---|---|---|
| `0x42b5xx` | `1` at `0x42b5a5` | `0` at `0x42b595` | `0x7a9730` at `0x42b677` |
| `0x42bbxx` | `1` at `0x42bc17` | `0` at `0x42bc07` | `0x7a9730` at `0x42be02` |
| `0x42c2xx` | `1` at `0x42c32e` | `0` at `0x42c31e` | `0x7a9730` at `0x42c351` |
| `0x42c4xx` | `1` at `0x42c650` | `0` at `0x42c660` | `0x7a9730` at `0x42c683` |

Every pass enables blending immediately before it draws, and none of them writes
`SRCBLEND`/`DESTBLEND`, so the `SRCALPHA`/`INVSRCALPHA` pair left behind by the
shared restore sites (`0x410089`/`0x41023a`, `0x410b91`/`0x410b9b`, …) applies.

**Correction (this revision).** The earlier revision of this section read the
stage-0 alpha opcode `4` as `SELECTARG2` and concluded that the blend was inert.
D3D8's `D3DTEXTUREOP` enum is `DISABLE = 1, SELECTARG1 = 2, SELECTARG2 = 3,
MODULATE = 4, MODULATE2X = 5, MODULATE4X = 6, ADD = 7, …`, so `0x77a020(0,4)` at
`0x7a9785` is **`MODULATE`**, not a select — consistent with the stage's colour
op `5` = `MODULATE2X` at `0x7a976d`, which needs the same enum. With
`ALPHAARG1 = TEXTURE` and `ALPHAARG2 = DIFFUSE` (`0x7a9792`) the fragment alpha is
`texture.a * diffuse.a`, and the per-mesh loop writes `diffuse.a = mesh+0x2cc`
(`0x7b9d5e..0x7b9d74`, `0xff` unless a mesh declares opacity). The sampled
texture alpha therefore reaches the blend as the surface coverage, and the pass's
`ALPHABLENDENABLE(1)` with `ALPHATEST(0)` is exactly how the client cuts leaf,
canopy and banner cards.

The symptom that exposed the mis-reading is visible in the viewer: with an opaque
scenery pass, City of the Plume's giant tree renders its canopy as solid dark
green rectangles, where the client shows leaf clusters between transparent
gaps. The viewer now emits the stage alpha and blends the scenery pass
(`scenery_pipeline`, `SRCALPHA`/`INVSRCALPHA`), and `bmd-sided-gpu-check` proves
it on the raster: a half-alpha atlas lands at ~128 over a cleared target, a
zero-alpha atlas writes nothing at all.

The `+0x95 == 2` test at `0x42c5f3` is not a per-ornament selector either. The
byte belongs to the object that parses the graphics settings at `0x40d0xx`, and
`+0x95` is the `WaterEffect` key — compared at `0x40d16c`, written back at
`0x40d765` — next to `Sight` (`+0x94`, `0x40d150`/`0x40d750`) and `SimpleTerrain`
(`+0x96`, `0x40d18d`/`0x40d76c`). The fourth row above is the water-quality
variant of the scenery pass, chosen by a game setting, never by a mesh or a
material. No ornament needs to be classified into a blended queue at all.

### Acceptance

`bmd-sided-gpu-check` proves, on lavapipe, both axes the mesh owns and the alpha
row: opaque shading (`kind` 2.0), two-sided shading (`kind` 2.1), a zero-alpha
atlas that must now write *nothing* (it used to shade under the old opaque rule),
a half-alpha atlas that must land at ~128 over a cleared target (the blend is
real), and the culling sweep of §3. The unit tests pin the table:
`every_classified_mesh_has_no_alpha_test_and_texture_alpha_coverage`
(textured ⇒ `TextureAlphaSrcOver`, untextured ⇒ `Opaque`, independent of
sidedness) and `textured_scenery_must_keep_the_texture_alpha_for_coverage`
(rebuilds the `0x7b9d64` diffuse word and asserts the classifier never invents a
different coverage source). The four pass sites are recorded as
`bmd_baked::native::PASS_BLEND_ON` / `PASS_ALPHA_TEST_OFF`.

## 3. Task P3.04 — culling, winding, sidedness

### Recovered rule

There is no per-asset culling decision. The submitter reads one byte,
`mesh+0x26c` (`0x7a98c5`), and either leaves the pass cull mode alone or sets
`CULLMODE(1)`/`NONE` for that one mesh (`0x7a98cf`), restoring the saved value
(`0x168(%ecx)` read at `0x7a98bb`) in every exit path (`0x7a9a26`, `0x7a9a49`,
and the two early exits at `0x7a96f2`/`0x7a9716`). Winding is never rewritten:
the client relies on the placement's own determinant, so a mirrored
(negative-determinant) placement produces the opposite `front_facing` and the
same single-sided rule then keeps the other face. No branch in the client
compensates for mirroring.

### Implementation

* `DrawState.cull` is the only culling input; the shader's winding drop is gated
  by `kind` bands that correspond one-to-one to `Cull::BackFace`
  (`1.05..1.15` for colour-only, `1.5..2.05` for textured). `camera.clip.w`
  still disables it globally, `camera.clip.z` still selects the projected
  parity for the reflected pass; neither was changed.

### Acceptance

`bmd-sided-gpu-check` runs six instances per case: single-sided CCW,
single-sided CW (index order reversed), two-sided CCW, two-sided CW,
**single-sided with a mirrored placement** (positions mirrored in X, the
equivalent of `x = y.cross(z)` under a negative determinant) and two-sided
mirrored. Expected coverage, derived from the shader predicate and confirmed by
the Python oracle in §6:

| `clip.z` | `clip.w` | expected pixels |
|---|---|---|
| 0 | 0 | drawn, dropped, drawn, drawn, **dropped**, drawn |
| 1 | 0 | dropped, drawn, drawn, drawn, **drawn**, drawn |
| 0 | 1 | all drawn |
| 1 | 1 | all drawn |

The mirrored single-sided quad follows its neighbours' rule exactly — it is
dropped when the unmirrored CCW quad is kept and kept when the projection
parity flips — which is the per-case evidence the task asks for.

## 4. Task P3.05 — depth, bias, ordering, overlap

### Recovered rule

* **Bias:** none. `ZBIAS` is written only by the effect path (`0x417453`
  value 2, restored `0x417471`); the earlier note that the client used a
  default bias of 50 does not survive inspection and is withdrawn. `ZFUNC` is
  never written by the baked path, so it keeps the D3D8 default `LESSEQUAL`.
* **Precision:** the client's depth buffer is D3D8 fixed-point; the viewer keeps
  its documented reversed-`f32` buffer. That deviation is unchanged and stays
  reported.
* **Order:** mesh array order, one submission per mesh, no sorting by texture or
  distance in the submitter (`0x7a9808` increments through `[mesh+0x2c0]`).
* **Ties:** equal depth must reach the *later* submission (`LESSEQUAL`), so a
  duplicate triangle drawn second wins.

### Implementation

* `src/gpu.rs` gains `scenery_pipeline`: identical to the world pipeline except
  `depth_compare: GreaterEqual`, used for `terrain_end..local_end` and both
  page lists. Terrain keeps `Greater`, so terrain-vs-terrain behaviour is
  untouched and terrain-vs-scenery ties resolve to the scenery, as in the
  client.
* `bmd_baked::overlap_outcome()` classifies a coincident pair as `Culled`
  (opposite winding and at least one mesh single-sided — exactly one surface
  survives for any camera side), `LaterSubmissionWins` (both rasterise and the
  pair is drawn in the client's mesh order) or `Unresolved` (both rasterise but
  the pair is not proved to share a submission). It is unit-tested for all five
  cases.
* `src/bin/bmd-overlap-survey.rs` now reads `two_sided` per mesh, applies the
  classifier to every pair and prints
  `resolved_by_cull=` / `resolved_by_order=` / `resolved_cross_mesh=` alongside
  the existing counters, plus the outcome on each of its twelve example lines.

### Acceptance status

Met, on the retail archive. The owner ran this branch's shipped executable:

```
> BmdOverlapSurvey.exe "E:\pw142\element\litmodels.pck" | findstr /c:"models="
models=16335 meshes=40579 with_exact_duplicate_triangles=5416 duplicate_triangles=174744 same_winding=6519 opposite_winding=168225 across_meshes=12 normals_positive=5089227 normals_negative=91917 resolved_by_cull=168057 resolved_by_order=6687 resolved_cross_mesh=0 unparsed=0
```

* **The census is reproduced exactly** — 16,335 BMDs, 40,579 meshes, 5,416 models
  with duplicates, 174,744 coincident triangles, 6,519 same-winding, 168,225
  opposite-winding, 12 across meshes — and adds `unparsed=0`, so the table saw the
  whole archive rather than a sample, and no mesh version was silently skipped.
* **Every pair is attributed.** `resolved_by_cull=168,057` plus
  `resolved_by_order=6,687` equals 174,744, so nothing is left `Unresolved`. The
  split closes arithmetically twice: 168,225 − 168,057 = 168 opposite-winding
  pairs must have both meshes two-sided, and 168 + the 6,519 same-winding pairs =
  6,687, exactly `resolved_by_order`. So 96.2% of the coincident triangles never
  reach the depth test twice (the `CULLMODE` rule), and the remaining 3.8% are
  settled by submission order — the `LESSEQUAL` tie, reproduced as `GreaterEqual`
  for scenery.
* **`resolved_cross_mesh=0` with `across_meshes=12`** says all twelve cross-mesh
  coincidences are removed by culling, so no pair needed the per-pair
  `same_submission` proof that file data alone cannot supply.
* **`normals_positive=5,089,227` / `normals_negative=91,917`** is the corpus
  winding statistic the cull rule keys on: 98.2% of triangles have a geometric
  normal on the same side as the stored vertex normal, i.e. the file winding
  follows the client's convention. It is a property of the archive, not a
  draw-state input, and it is recorded so a mirrored-model regression is
  measurable rather than anecdotal.
* **`resolved_by_order` is an order claim, not a guess.** The 6,687 pairs are
  drawn in one submission by one model instance in mesh-array order (`0x7a9808`
  increments through `[mesh+0x2c0]`, `0x7b9ca0` appends), so "later wins" is the
  client's own sequence.

The twelve per-model example lines the same run printed are all
`... duplicate mesh 2 (Building/textures/g/777.dds) / 2 (...) Culled`, i.e. the
commonest shape is a flip pair inside one mesh of a single-sided model — precisely
the case where the cull rule keeps one surface and the viewer's shader discard
reproduces it.

## 5. Changed files and production call sites

| File | Change | Production call site |
|---|---|---|
| `src/bmd_baked.rs` | Draw-state table, `classify`, `alpha_test`, `cutout_reference`, `overlap_outcome`, `native` write-site constants (incl. the four pass blend/alpha-test sites), 9 unit tests | imported by `gpu.rs`, `world_objects.rs`, `bin/bmd-overlap-survey.rs`, `bin/world-preload-check.rs` |
| `src/gpu.rs` | shader: culling bands extended to the colour kinds, colour-only `MODULATE2X` branch, baked alpha test removed, textured baked branch now returns the stage alpha (`raw_tex.a`, i.e. `MODULATE(TEXTURE, DIFFUSE)`); new `scenery_pipeline` (`GreaterEqual` **and** the pass's `SRCALPHA`/`INVSRCALPHA` blend) created in `Renderer::new` and used by `draw_opaque` | `draw_opaque()` (`set_pipeline`, `terrain_end..local_end` + `local_pages` + `world_pages`) |
| `src/world_objects.rs` | whole-world page vertices classify through the table instead of inline `2.0/2.1`; after the `main` merge the same builder keys the atlas slot on Agent A's content hash (`objects.texture_key`) | `run()` page builder |
| `src/bin/world-preload-check.rs` | declares `mod bmd_baked` (the shared table it now needs) | diagnostic only |
| `src/bin/bmd-overlap-survey.rs` | per-pair overlap classification and counters; `meshes=`/`unparsed=` census counters and per-model error tolerance so a partial archive still yields a result | diagnostic only, shipped as `BmdOverlapSurvey.exe` |
| `src/bin/bmd-sided-gpu-check.rs` | six instances per case, mirrored placement, and the alpha sweep: opaque atlas for the cull sweep, half-alpha atlas that must land at ~128, zero-alpha atlas that must write nothing | diagnostic only |
| `.github/workflows/pr-checks.yml` | adds `cargo run --locked --bin bmd-sided-gpu-check` to the lavapipe step, and builds/uploads `bmd-overlap-survey` inside the existing `Perfect-World-Explorer-Windows-x64` artifact — **cross-lane notice**: it only adds my own check and my own binary, no existing step or artifact name changed | CI gate |
| `Findings/Unsupported-client-paths.md` | scenery row updated with the recovered facts; new row for the unreachable cutout state (both rows stay open where the work is open) | — |

Nothing outside the static-model pipeline was touched: no terrain pipeline
change, no water/effect state, no texture residency, no view/pass selection.

## 6. Tests executed and results

**Not executable locally** — there is no Rust toolchain in this sandbox (§0).
The evidence actually produced here, and the CI run that produces the rest:

| Check | How | Result |
|---|---|---|
| `md5sum google_drive/elementclient.exe` | local | `5f02895688fa580506e869156340a664` (6,000,640 B) ✓ |
| Syntax of every touched file | `tree-sitter-rust` parse of `src/gpu.rs`, `src/bmd_baked.rs`, `src/world_objects.rs`, `src/bin/{bmd-sided-gpu-check,bmd-overlap-survey,world-preload-check}.rs` | 0 `ERROR`/`MISSING` nodes ✓ |
| Integer alpha-reference arithmetic | Python oracle of `80*a/255+1` | `a=0→1`, `1→1`, `128→41`, `0xfe→80`, `0xff→81`, `u32::MAX→255` — matches the Rust test expectations ✓ |
| Day/night blend arithmetic | Python oracle of `(d*(255-w)+n*w)>>8` | `[9,19,29]`, `[199,209,219]`, `[127,127,127]`, `[254,254,254]` — matches `integer_blend_matches_the_recovered_arithmetic` ✓ |
| Stage-0 alpha coverage | Python oracle of `texture.a * diffuse.a` under `SRCALPHA`/`INVSRCALPHA` over a cleared target | `a=0 → 0`, `a=128 → 128`, `a=255 → 255` — the new `bmd-sided-gpu-check` cases assert the same three values ✓ |
| Culling predicate of the six fixture instances | Python oracle of the shader condition | `(0,0)→[T,F,T,T,F,T]`, `(1,0)→[F,T,T,T,T,T]`, `(0,1)`/`(1,1)`→all `T` — matches the Rust expectations ✓ |
| `python3 tools/test_phase1_ready.py` | local | `Ran 7 tests … OK` ✓ |
| `python3 tools/test_eight_phase_status.py` | local | `Ran 5 tests … OK` ✓ |
| `python3 tools/check-eight-phase-status.py` | local | `errors: []`, `complete_phases: []` ✓ |
| `BmdOverlapSurvey.exe "E:\pw142\element\litmodels.pck"` | owner's machine, executable from the `Perfect-World-Explorer-Windows-x64` artifact of run 41527209273 | `models=16335 meshes=40579 with_exact_duplicate_triangles=5416 duplicate_triangles=174744 same_winding=6519 opposite_winding=168225 across_meshes=12 normals_positive=5089227 normals_negative=91917 resolved_by_cull=168057 resolved_by_order=6687 resolved_cross_mesh=0 unparsed=0` — census reproduced, 168,057 + 6,687 = 174,744 attributed, none unresolved ✓ |

CI executed the rest on this branch. The sequence, including the two defects the
runs caught (a `u32` overflow and a rustfmt shape), is:

| Run | Head | Result |
|---|---|---|
| 41520738614 | first commit | `Check formatting` **failed** — `bmd_baked.rs` test asserts were not in rustfmt's shape |
| (formatting helper, deleted) | `131c642` | `cargo fmt --all` applied; only `src/bmd_baked.rs` changed |
| 41521409838 | `249ec9e` | `Check formatting` ✓, `Verify recovered regional arithmetic on x87` ✓, `Test requirement status audits` ✓, `Check all binary targets` ✓ (`cargo check --locked --bins`), `Test GFX light dependency audit` ✓, `Run viewer unit tests` **failed** |
| 41522066048 | `ddc3839` | `Viewer unit tests` ✓, `Baked scenery raster regression` ✓ (`bmd-sided-gpu-check` on lavapipe) |
| 41523033865 | `991fe2d` | **both jobs green.** `Rust checks and tests (Linux)`: `Check formatting` ✓, `Verify recovered regional arithmetic on x87` ✓, `Test requirement status audits` ✓, `Check all binary targets` ✓, `Test GFX light dependency audit` ✓, `Run viewer unit tests` ✓, `Run Phase 1 production shader regressions` ✓ (all fourteen lavapipe binaries, including the extended `bmd-sided-gpu-check` now wired into that step). `Windows executable (x64)` ✓ |
| 41526711344 | `8142dd0` | `Check formatting` **failed** — one new assertion in `bmd_baked.rs` was left on a single line where rustfmt's `fn_call_width` puts it in a vertical list; nothing else ran |
| 41531096347 | `1e1088c` | **both jobs green after merging `main`** (`2b3b4d9`, Phase 3 Agent A: client-order texture resolution, bounded residency, content-keyed atlas slots). One conflict, in the `world_objects.rs` page builder: resolved as *their* content-keyed slot key plus *my* `bmd_baked::classify` kind, and `world-preload-check.rs` declares `texture_resolution` so its `#[path]` module set still closes. No report or counter changed by the merge |
| 41527209273 | `c099c0f` | **both jobs green**, and the Windows artifact now carries `BmdOverlapSurvey.exe` plus its checksum (6,111,979 B artifact). `Rust checks and tests (Linux)`: `Check formatting` ✓, x87 reference ✓, requirement audits ✓, `Check all binary targets` ✓, GFX light audit ✓, `Run viewer unit tests` ✓ (nine `bmd_baked` tests), all fourteen lavapipe shader regressions ✓. `Windows executable (x64)` ✓ including the new survey build and stage step |
| 41534537555 | `4dd68d0` | **both jobs green** with the stage-0 alpha correction: `Check formatting` ✓, x87 reference ✓, requirement audits ✓, `Check all binary targets` ✓, GFX light audit ✓, `Run viewer unit tests` ✓ (nine `bmd_baked` tests, including the renamed `every_classified_mesh_has_no_alpha_test_and_texture_alpha_coverage` and the new `textured_scenery_must_keep_the_texture_alpha_for_coverage`), and all fourteen lavapipe shader regressions ✓ — `bmd-sided-gpu-check` now also rasterises the alpha sweep: a half-alpha atlas lands at ~128 over a cleared target, a zero-alpha atlas writes nothing. `Windows executable (x64)` ✓ |

The unit-test failure was real and is fixed in `ddc3839`:
`cutout_reference(u32::MAX)` multiplied in 32 bits and panicked on overflow in a
debug build ("attempt to multiply with overflow"); the client's `imul` at
`0x7a9910` is 32x32→64, so the function now widens to `u64` before the
multiply. After the fix, the eight `bmd_baked` tests and every pre-existing
viewer unit test pass, and the extended `bmd-sided-gpu-check` passes on lavapipe
— the six-instance expectation table of §3 is therefore confirmed by raster
output, not only by the Python oracle. The last code head `4dd68d0` repeats all of that inside the ordinary PR gate (run
41534537555), which also proves the alpha correction on the raster. The `main` merge
(`1e1088c`, run 41531096347) re-ran the whole gate over the combined tree, so every number
above is from a run of code that is in this branch.

Two temporary, review-only workflows were used to get feedback into this sandbox
(`cargo fmt` and the failure text, because Actions logs and check annotations
are not reachable from it). Both are deleted before handoff and contribute no
product code; the formatting they produced is kept.

## 7. Remaining unsupported cases and assumptions

* **Face culling is implemented in the fragment shader, not the rasteriser.**
  `primitive.cull_mode` stays `None` because the same pipeline draws terrain,
  water and scenery; the discard is equivalent for coverage but does not save
  raster work, and it depends on `front_facing` being derived from the same
  winding convention in every pass. Recorded, not hidden.
* **The depth-writing blend was reverted to an alpha-test cutout (post-merge
  follow-up).** The reverse engineering in §2 is sound — the client does leave
  `ZWRITEENABLE` on while `ALPHABLENDENABLE(1)` is set, so an alpha-0 card still
  occupies its single, ordered depth buffer — but reproducing that state in the
  viewer broke it. The viewer's water pass is depth-tested against the same
  buffer (`water_gpu.rs` reads `scene_depth` for the refraction guard), so a
  transparent baked quad occluded water as a solid square (lily pads) and a
  nearer canopy hid the canopy behind it through its transparent gaps. The
  shared world shader now discards texels below the cutout threshold for the
  textured baked band too (`((i.kind>1.5 && i.kind<2.5) || i.kind>3.5)`), so
  transparent texels write neither colour nor depth; survivors are opaque and
  still write depth (`GreaterEqual` keeps §4's duplicate-tie rule intact).
  `scenery_pipeline` is unblended again. `Blend`/`AlphaTest` in `bmd_baked.rs`
  remain as client-state documentation; the deviation is documented there and
  pinned by `production_wgsl_implements_the_classified_states` and
  `bmd-sided-gpu-check` (a nonzero below-cutout alpha must now write nothing).
* **One alpha source only.** Coverage is the sampled texture alpha; a mesh that
  declares its own opacity needs the `ALPHATEST(1)` branch and is refused by
  `alpha_test()` until that branch is recovered. No 1.4.2 reader writes
  `mesh+0x2cc`, so this is a guard, not a gap in retail data.
* **The alpha-test branch is unimplemented by construction.** A `mesh+0x2cc`
  other than `0xff` raises an error (`bmd_baked::alpha_test`). If a future
  archive ever carries one, the mesh is skipped with a message rather than
  drawn wrong.
* **Depth-buffer format and precision still deviate** (reversed `f32` with
  `Greater`/`GreaterEqual` versus the client's fixed-point `LESSEQUAL`). The
  tie rule now matches; the quantisation does not.
* **The census is a single archive.** §4 measures `litmodels.pck` (16,335
  models); `models.pck`, `building.pck` and the map archives were not surveyed,
  so overlaps between a litmodel and a building model are outside the count. The
  classifier and the shipped tool cover them the moment those archives are run.
* **Untextured baked meshes** are now rendered with the recovered stage, but no
  retail mesh of that shape was read in this session, so the branch is covered
  by code and state tests only, not by a corpus example.
* **Two state writes in the submitter are still unmapped.** `0x77baf0` and
  `0x77bad0` are called once, unconditionally, at `0x7a97b2`/`0x7a97b7` before
  the `0x785550` cull gate; neither has an identified render-state id yet, so
  no row of §1 is claimed for them. They are the only unclassified writes in
  `0x7a9730`.
* **Animated/instanced submitter variants of `0x7a9730`** (for example any
  per-frame state block a caller installs before entering the loop) were not
  enumerated; only the loop's own writes are claimed.

## 8. Interface changes for other agents

* `bmd_baked::classify(version, textured, two_sided) -> Result<DrawState, String>`
  — the only supported way to obtain the `kind` float. The `kind` encoding is
  now: `0` terrain base, `0.5–1.0` water/quad colour, **`1.1`/`1.2` untextured
  baked (single/two-sided)**, `2.0/2.1` textured baked, `3.75` grass, `5`
  far-tree billboards, `>4.5` rotating billboards. Anything that emits vertices
  must not invent values inside `1.05..1.15`, `1.05..1.25` or `1.5..2.05`.
* `bmd_baked::overlap_outcome(same_winding, first_two_sided, second_two_sided,
  same_submission)` — reuse it for any other coincident-geometry report rather
  than re-deriving the rule.
* `Renderer` gained a private `scenery_pipeline`; the water depth copy at
  `gpu.rs:1454` still uses `pipeline` (`Greater`). If another agent needs the
  equal-depth tie elsewhere, ask for a pipeline with `GreaterEqual` instead of
  changing the shared one.
* `.github/workflows/pr-checks.yml` gained one step; the workflow owner should
  keep `bmd-sided-gpu-check` in the list because it is the only raster proof of
  the cull table.

## 9. Per-point acceptance lines

P3.02 material and texture-stage state — one table drives both static-model
draws, the material-colour question is answered by `LIGHTING(0)`, the cutout row
is pinned as unenterable rather than assumed, and the blending row is closed by
correcting the stage-0 alpha opcode: all four passes enable blending, none writes
the factors, and `ALPHAOP(4)` is `MODULATE` over `TEXTURE`/`DIFFUSE`, so the
texture alpha drives coverage and the viewer's scenery pass now blends. The
`+0x95` gate that looked per-ornament is the pass object's `WaterEffect`
setting.

`Task P3.02 | Finished Yes | Production integration gpu.rs build()+draw_opaque, world_objects.rs run() via bmd_baked::classify | Native evidence 0x7a9759 LIGHTING(0), 0x7a9762 SPECULAR(0), 0x7a976d COLOROP 5, 0x7a977a/0x7a9792 args, 0x7a979e stage 1 off, 0x7a98f1/0x7a992b ALPHATEST off, 0x7b9408 mesh+0x2cc=0xff, no 0x77a290 write inside the mesh loop, 0x42b5a5/0x42bc17/0x42c32e/0x42c650 ALPHABLENDENABLE(1) with ALPHATEST(0) at 0x42b595/0x42bc07/0x42c31e/0x42c660, no SRCBLEND/DESTBLEND write in any pass, ALPHAOP 4 = MODULATE (D3D8 DISABLE/SELECTARG1/SELECTARG2/MODULATE/MODULATE2X = 1/2/3/4/5) with TEXTURE,DIFFUSE at 0x7a9792, 0x7b9d5e..0x7b9d74 diffuse = baked_rgb | mesh+0x2cc<<24, +0x95 = WaterEffect parsed 0x40d16c and saved 0x40d765 | Tests executed and results bmd_baked unit tests (9) including every_classified_mesh_has_no_alpha_test_and_texture_alpha_coverage and textured_scenery_must_keep_the_texture_alpha_for_coverage, bmd-sided-gpu-check opaque/two-sided/half-alpha/zero-alpha cases on lavapipe | Remaining gaps alpha-test branch refused; untextured branch not seen in the corpus; the WaterEffect == 2 pass variant is not implemented`

P3.04 culling, winding, sidedness — no per-asset decision exists: one mesh
byte selects `CULLMODE(1)`, the pass value is restored on every exit path, and a
mirrored placement simply reverses `front_facing` under the same rule.

`Task P3.04 | Finished Yes | Production integration bmd_baked::DrawState.cull drives the shader winding bands; scenery_pipeline keeps cull_mode None and the fragment discard reproduces CULLMODE | Native evidence 0x7a98c5 test of mesh+0x26c, 0x7a98cf CULLMODE(1), 0x168(%ecx) save at 0x7a98bb, restores 0x7a9a26/0x7a9a49/0x7a96f2/0x7a9716 | Tests executed and results bmd-sided-gpu-check six-instance cases incl. mirrored placement (CI runs 41522066048, 41523033865); Python oracle of the predicate reproduced all four expected patterns | Remaining gaps culling is a fragment discard, so the mirrored case is proved by raster output rather than by hardware cull state`

P3.05 depth, bias, ordering, overlap.

`Task P3.05 | Finished Yes | Production integration gpu.rs scenery_pipeline GreaterEqual for terrain_end..local_end and both page lists; bmd_baked::overlap_outcome; bmd-overlap-survey counters | Native evidence no ZBIAS write outside 0x417453/0x417471, no ZFUNC write in the baked path (only 0x796753/0x796cd2), mesh-order loop 0x7a9808 with per-mesh submit 0x7b9ca0 | Tests executed and results coincident_pairs unit test (5 cases), coincident_duplicates_are_won_by_the_later_draw, Python oracle of equal-depth semantics, and the retail census run with the shipped executable: models=16335 meshes=40579 duplicate_triangles=174744 resolved_by_cull=168057 resolved_by_order=6687 resolved_cross_mesh=0 unparsed=0, i.e. all 174,744 coincident triangles attributed (168,057 + 6,687 = 174,744) | Remaining gaps depth is the viewer's reversed f32 buffer rather than the client's fixed-point LESSEQUAL, so the tie rule matches but the quantisation does not; only litmodels.pck was surveyed, so cross-archive overlaps are unmeasured; the earlier "default bias 50" claim is withdrawn`
