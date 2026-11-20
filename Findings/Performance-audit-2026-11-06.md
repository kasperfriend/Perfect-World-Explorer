# Performance audit — 2026-11-06

Four things here: the reported default-marker bug on the time-pass speed slider,
a sweep of the whole viewer for work that can be removed without changing a
single visible pixel, the reported lens flare sitting slightly above the sun, and
— at the far end of that sweep — the scenery draw fold plus an assessment of what
a fully GPU-driven viewer would actually require (§5).

Every optimisation below is an allocation that is now reused, a lookup that is
now indexed, an environment read that is now done once, a worker that now runs
beside the loader instead of in front of it, or a batch of draws that now covers
the same triangles in the same order in fewer calls; no drawing rule, shader
constant, LOD threshold, draw order or texture resolution is touched. The flare
item (§4) is a correction: the chain was anchored on the point the sun's quad is
emitted from rather than on the disc the quad paints.

## 1. The slider default marker sat at the wrong value

*Reported:* the time-pass speed slider's default tick sat at 0.1 instead of at
1.0.

`marked_slider` painted the "default" tick with a **linear** fraction on every
slider, including the egui sliders that carry `.logarithmic(true)`. egui spreads
a logarithmic track by `ln(value)`, so the `1.0` default of the
`0.1..=600` speed range landed at `(1.0 - 0.1) / (600 - 0.1) = 0.0015` — the far
left of the track, which reads as "the default is 0.1". Flight speed and mouse
sensitivity, which are also logarithmic, had the same defect.

Fixed by moving the arithmetic into `slider_mark_fraction`, which uses the same
mapping egui uses (`ln(default / min) / ln(max / min)`) when the track is
logarithmic, and by passing the logarithmic flag for the time-pass slider, which
was the only call site that declared it on the `egui::Slider` but not to the
painter. The fraction is also guarded against a degenerate range
(`max <= min`) and non-finite input, where it now falls back to the left edge
instead of producing a NaN x-coordinate. Unit tests pin the three cases: the
logarithmic position of `1.0` in `0.1..=600` (0.265), the linear position, and
the degenerate range.

Nothing else about the slider changes: same label text, same tick colour, same
geometry.

## 2. Applied optimisations

| # | where | before | after | why it is invisible |
| --- | --- | --- | --- | --- |
| 1 | `objects::TextureCache` | `evict_unpinned` removed the evicted hash's names with `by_name.retain(..)` — a full scan of every reference name in the cache (tens of thousands during a whole-world load) for **every** evicted texture, on every store past the budget | a `aliases: HashMap<u64, Vec<String>>` reverse index; eviction drops that hash's own names | the same names are removed; the resolved-name set is identical. A defensive `retain` fallback keeps the exact answer if the two tables ever disagreed |
| 2 | `main::terrain` block loop | seven fresh `Vec`s per block (decoded heights, narrowed values, decoded normals, narrowed normals, colour section, and the grid-sized `heights`/`normals` handed to the builder) | hoisted into a `BlockScratch` that is `clear()`ed per block, one per decoding worker (see (8)) | same values, same order, same builder calls; only the allocations are reused |
| 3 | `terrain_residency::visible_runs` | a fresh `Vec` per view per frame (main, reflection, above-water, minimap) | `visible_runs_into` refills a caller-owned buffer; `visible_runs` stays as the owned wrapper | the buffer is emptied first and refilled in span order, so the runs are byte-identical (test: `visible_runs_into_reuses_the_buffer_without_keeping_the_old_answer`) |
| 4 | `terrain_gpu::visible_far_runs` | same, for the far mesh | `visible_far_runs_into`, same buffer rule | identical runs |
| 5 | `far_trees_gpu::update_resident_lods` | three fresh `Vec`s **per tree group** on every frame the camera moves | one trio for the whole pass, cleared per group | same instances, same batch ranges, same order. The buffers stay one-group sized, so the "do not keep the world-sized staging allocation" rule for `staging` is untouched |
| 6 | `gpu` UI | the FX status label built its long "Loader queue" tooltip string — two nested `format!`s and two `collect::<String>()`s — on every frame the menu was open | built only on the frame the label is hovered | the tooltip text and its `on_hover_text` path are unchanged |
| 7 | `gpu` hot paths | `std::env::var_os` on every mouse-motion device event, every opaque draw pass (twice) and every frame in the water/UI paths — each call takes the process environment lock | `env_switch` over `OnceLock`, resolved once per switch | the environment cannot change while the viewer runs; the same flags are answered |
| 8 | `main::terrain` block decode | one thread decoded a whole tile: every block's zlib inflate, delta walk, normal decode and vertex-colour check ran back to back on the loader thread, which is the dominant term of a cold start (and of the resident "All to VRAM" build) | `decode_block` is extracted and driven by `decode_blocks`: one worker per core (capped, `PW_TERRAIN_THREADS` pins it) decodes blocks independently, and the caller inserts them **in block order** as they line up, so the builder sees the same sequence as before | identical bytes reach `Builder::block` in identical order; only which thread produced them changed. Per-worker scratch replaces the old shared scratch, so the allocation count stays at two per block. `PW_TERRAIN_THREADS=1` restores the old loop exactly |
| 9 | `world_objects::GpuPage` scenery draws | one `draw_indexed` **per placement**, every frame, in every view. The submission order is frozen at load time, so a whole-world load re-issued thousands of draws per pass (main, refraction, above water, reflection) | the frozen order is folded once into `DrawCmd` spans, one per page run: touching placements of the same page collapse into a single draw, and locally streamed pages fold the same way through `fold_page_spans` | a folded span draws exactly the same indices in exactly the same order. A span is only ever extended across a placement that *touches* it, so a placement a view hides leaves a gap the fold refuses to cross (tests: `a_page_run_folds_into_one_draw`, `a_hidden_placement_splits_the_run_and_is_never_drawn`, `a_local_page_folds_its_visible_placements_and_skips_the_hidden_ones`, and `the_fold_draws_exactly_what_the_per_placement_loop_drew`, which compares the folded index sequence against the per-placement loop for every view) |
| 10 | `far_trees_gpu` LOD selection | the CPU walked **every** placement each frame the camera moved: distance, `tree_lod::select`, write the instance slot, then re-upload the whole instance buffer (three level slices per placement — 9.6 MB at whole-world scale, every frame) | the placement table (position + group index), each group's height and slice, and the per-batch `DrawIndexedIndirectArgs` are uploaded once at load; a single 64-wide compute dispatch then writes the instances and bumps the per-batch counts; `draw` is then one `multi_draw_indexed_indirect` for the whole batch list when the adapter offers `MULTI_DRAW_INDIRECT` + `INDIRECT_FIRST_INSTANCE` (each record's `first_instance` selects its slice, so the vertex buffer is bound once), and one `draw_indexed_indirect` per batch otherwise — a whole-forest draw is one call per pass in either case, where it used to be one per placement | the shader is `tree_lod::client_distance` and `tree_lod::select` transcribed operation for operation — the same bit-trick square root, the same 10 % windows around `height * 5 * scale` and `height * 16 * scale`, tested in the same order. `tree-lod-gpu-check` reads the per-batch counts and the `(fraction, mode)` record back and compares both against the CPU `select` for every frame, including two frames placed inside the transition windows where the cross-fade is non-trivial, and renders every non-transition frame twice — once from the CPU packer's buffer, once from the compute pass's — asserting the pixels are identical. The check's own single tree type was not enough: the per-group table scaled its record base by three (`first_batch * 3`) instead of using the batch index, so only group 0 was right — a whole-world cache lost its far trees and drew other groups' level records. The base is `first_batch` again, `the_group_table_points_every_level_at_its_own_record` pins the whole (group, level) layout, and the check now runs three groups of different heights so a mis-based table cannot pass |
| 11 | `grass_instances::Gpu::draw` | one `draw_indexed` **per grass batch** — one per (area, material) pair — every frame, in every pass that draws grass | the ranges are fixed until the next streaming update, so they are uploaded once as indirect records; the whole field is then one `multi_draw_indexed_indirect` per pass when the adapter has `MULTI_DRAW_INDIRECT` + `INDIRECT_FIRST_INSTANCE`, and the identical per-batch loop otherwise | both paths bind the same three buffers and draw the same ranges in the same order. `grass-instance-gpu-check` renders the per-batch path, the single-call path and the CPU-expanded reference for every clip/fog combination and asserts all three frames are identical. The record type is shared with the tree batches (`indirect_args`), and a unit test pins its 20-byte layout |
| 12 | `terrain_gpu::draw` | one `draw_indexed` **per visible run**, twice per frame per pass — the block walk hands over tens to hundreds of contiguous index ranges and each became its own call, for the near and the far mesh | the runs are turned into one indirect record each in `prepare_runs` (a few kilobytes, once per view) and each mesh is then a single `multi_draw_indexed_indirect` | every record reproduces `draw_indexed(start..start + count, 0, 0..1)` exactly — unit test — and the single-call path is only taken for the *same* list that was uploaded (`can_multi_draw`, unit-tested): a caller that does not call `prepare_runs`, or a view whose runs changed, falls back to the per-run loop, which is the code CI has been running all along |
| 13 | `draw_opaque` terrain runs | the block walk culled only the main view, so the underwater refraction, the above-water refraction and the water reflection each submitted **every** index of both meshes — millions of far-mesh indices (≈8.6 M at whole-world scale) plus the whole resident near mesh, three extra times per frame | each pass now declares the view it draws through (`ViewRuns::Main`/`Reflected`/`Full`) and submits that view's runs: both refraction passes are queued as `refracted = normal`/`above = normal`, so they cull with the main list; the reflection culls with `matrix * flip`, rebuilt per water height because the mirror plane is the water level; the top-down map keeps the full list | only a block whose eight corners all fall outside the same clip plane is dropped, and the test runs in clip space, so the mirrored matrix is culled like any other view — `the_reflection_view_culls_through_the_mirrored_matrix` pins that the direct and mirrored views keep opposite halves of the same span list. Every submit re-queues its own records first, so no pass can inherit the previous view's list, and a view that keeps nothing uploads an empty list, which the per-run loop draws as nothing |
| 14 | `render_minimap` terrain | the realistic minimap is a 320×320 top-down window of `±minimap_range` — 256 game units by default, 512 across — and yet it submitted **every** block of both meshes: the whole resident near mesh and the whole-world far mesh, recoloured for a square a few hundred units across | the map pass draws through its own orthographic matrix, so it now queues `visible_runs` for that matrix like any other view (`ViewRuns::Map`); the full-world map in the World tab still submits the full list, and a zero range falls back to it | the same eight-corner clip-space test, and a block entirely outside the window cannot rasterize a pixel into the map. `the_minimap_window_keeps_only_the_blocks_under_it` pins the exact `minimap_matrix` shape — reversed near/far included — keeping the block under the eye and dropping one 5000 units away |

Measured expectations, not benchmarked frame times (no GPU in this sandbox):

* (1) is the big one for a paged load. A budget-sized cache over the shipped
  corpus evicts on most stores, and each eviction used to hash the entire name
  table.
* (2) removes ~7 allocations per terrain block — thousands per world load.
* (9) is the big one for a loaded whole world: the per-view command list is
  built once (three lists, one per visibility gate) and the frame then issues one
  draw per page run instead of one per placed object. Both the driver overhead
  and the per-frame CPU loop shrink by roughly the average placements-per-page
  ratio, and the merged spans are the foundation a GPU-driven (indirect) scenery
  pass would need.
* (8) is the big one for a cold start: the block loop is pure per-block work
  (inflate, delta walk, normal decode), so it scales with cores — 8 cores is
  roughly an 8x shorter terrain decode, and it is the phase that gates the
  resident "All to VRAM" build too.
* (3)–(5) remove per-frame allocations from the draw path; (6)–(7) remove
  per-frame and per-input string and environment work from the UI/event path.
* (13) is the one that compounds: the terrain is submitted five times per frame
  (main, both refraction passes, reflection, map) and three of those five used to
  hand the GPU the entire world no matter where the camera looked. They now
  submit the culled lists the main view already builds — the reflection gets its
  own, mirrored one — so a frame goes from five whole-world terrain submissions
  to four culled ones plus the map. (14) then takes the map out of the bill for
  the live minimap too: it is a window a few hundred units wide, and it used to
  be the most expensive view in the frame.

## 3. Candidates examined and deliberately not changed

| candidate | verdict |
| --- | --- |
| `minimap` pass every presented frame | Left alone. The comment on it is explicit: the 350 ms timer it replaced capped minimap motion under 3 FPS. It is a live top-down render of animated water and the moving clock, so any "skip when idle" rule would freeze visible motion. |
| `far_trees_gpu` `staging` reuse | Resolved by item (10): the CPU copy is gone. The compute pass owns the instance buffer, so there is nothing to re-compact, and the check-only packer allocates its staging per call instead of keeping a second world-sized copy resident for the session. |
| Per-frame `create_view` (scene depth, post chain, display gamma, present encode, surface) | Left alone. Caching the views is a few hundred nanoseconds per frame but every one of them has to be invalidated when its texture is recreated on resize, and a missed invalidation draws into a stale target — a real visual break for a tiny gain. |
| `terrain_lod::window_lod` scratch (`levels`, `cases`) | Half done. The per-frame "would the drawn mesh differ?" test now calls `window_levels`, which builds only the level grid and skips the neighbour walk that stitches the edge cases — the cases are only needed when the mesh is actually rebuilt, so the frame loop no longer pays for the 2500-byte table or the second pass over every cell. Turning the remaining 625-byte `levels` vector into a caller-owned buffer would need the buffer threaded through `LodState`, for no measurable gain. |
| `lod_window`/`LodState` per frame | Kept. The recovered `0x77e360` rule is evaluated for every resident block on every frame by design (it is what makes the window follow the camera), and the window is at most 25 x 25 blocks. |
| `terrain` in the full-world map (World tab) | Left alone. That map is a whole-range top-down view by construction, so its block list is the entire world; culling it would drop exactly what it exists to show. It keeps submitting the full index range, and the live minimap uses its own window instead (14). |
| Water `visible` batch list (clone + sort per frame) | Kept. The batch list is small and the sort is what gives back-to-front water blending; indices instead of clones would be a churn-only change. |
| `texture budget` / `terrain budget` `std::env::var` | Not hot: each is read once when the cache or the resident grid is created. |
| GPU instancing for animated world models, one-pass shadow/reflection merging, minimap caching | Out of scope: structural renderer changes, not "same pixels, less work". |
| `terrain_gpu::update_residency` `device.poll(Wait)` | Kept for now, and recorded as the next candidate. It runs on a mesh rebuild (a streaming update), not on every frame, and the wait is what retires the previous atlas and `FarGpu` bindings before the new ones are bound. Replacing it with an in-flight list is a queue-lifetime change across the whole renderer rather than a local edit, so it needs its own pass. |
| Water `queue.submit` per plane | Left alone. The reflection path reuses the refraction target between planes, so the submit boundaries are what order those passes; merging them needs explicit barriers and would save one or two submissions per frame over a handful of planes. |
| Terrain cache compression (`zlib` on every block write) | Kept: it is what makes the on-disk page object small. The encode now runs on whichever worker decoded that block (see (8)), so it costs a thread rather than the whole load. |
| Parallel texture decode in `world_objects::run` (32-id batches through `load_ids`) | Not taken. The decode itself is a good candidate, but the batch loop shares one `Objects` cache, one resident builder and one package file handle per batch; splitting it means a second handle per worker and a locked cache, which is a structural change to the streaming worker, not a local one. |

## 4. The flare chain sat a disc-radius above the sun

Not a performance item, but it was found while auditing the sky pass and is in
the same commit.

The sun's recovered template is a 20-unit quad emitted at `direction * 100` whose
vertices run `y 0 .. -20`, so the **emitted point is the quad's top edge** and the
painted disc hangs `half` (= `sun_ndc_half(FOV_DEGREES)`, 0.143 NDC at 70 degrees)
below it. `celestial` already applies that shift to its own `uv`, which is why
the disc and the sunset land where the client draws them. The flare chain did
not: `flare_lanes` returns the raw projected NDC of the emitted point and
`update` wrote it straight into `glow_center.zw`, so every ghost sprite was
anchored one disc-radius too high — the "flare is a bit above the sun" report.

`flare_anchor` now drops the NDC `y` by `half * (1 + 2 * night)`, mirroring the
`1 + 2 * n` swell `celestial` applies to the same quad, and `update` writes that
into `glow_center.zw` before the uniform is uploaded. The shader is unchanged:
it already reads `glow_center.zw` as the chain's anchor, and the occlusion taps
now sample the disc's centre instead of its top edge, which is where they belong.

Test: `the_flare_chain_anchors_on_the_painted_disc` (the anchor is one half
extent below the projected point, three at full night, clamped outside 0..1, and
a camera pointed at the sun anchors exactly `half` below the screen centre); the
existing `the_flare_chain_hangs_where_the_sun_projects` still pins the projection
itself.

## 5. What "the whole viewer on the GPU" would take

Worth stating plainly, because the question comes up: the *rendering* is already
GPU-only. Terrain (near and far LOD), scenery and skinned models, water surface,
refraction, reflection and shoreline edges, the sky (one fragment shader for the
panorama, both discs, the shine strip and the whole flare chain), clouds and
weather, grass, far trees, the post chain (fog, glow, gamma, sharpen), the
minimap and the full map all run in shaders. The CPU uploads per-frame uniforms
and issues commands; it never touches a pixel.

What is still on the CPU each frame, roughly in order of cost:

1. **Streaming** — `world_objects` decodes ornaments and uploads pages on its own
   thread, and `world_models_gpu::update_pass` advances poses, resolves
   attachments, applies activation, updates owners/the grid and rebuilds the
   alpha order.
2. **Scenery draws** — page binds and draws. Item (9) just took this from one
   draw per placed object to one per page run.
3. **Visibility** — `visible_runs_into` / `visible_far_runs_into` walk the
   resident block window per view, `window_lod` per block, the water batch
   filter/sort. Items (10) and (11) took the far-tree LOD selection and the
   grass draw list off this list.
4. **Frame math** — `begin_frame` (lighting, fog, sun/moon, glow parameters),
   a few hundred flops.
5. **egui** — the settings/HUD tree, tessellation and vertex upload. On a frame
   with the menu open this is usually the largest single CPU item, and it cannot
   move to the GPU without replacing the UI toolkit.

A GPU-driven successor would add, per subsystem:

| step | what it needs | why it is not here yet |
| --- | --- | --- |
| scenery/terrain culling on the GPU | per-view storage buffers of draw descriptors, a compute pass, `multi_draw_indexed_indirect` (optional feature) or one indirect draw per page | the submission order is load-bearing: coplanar depth ties are won by the first draw, so the compute pass must reproduce the frozen order exactly. Item (9)'s command lists are the CPU reference such a pass would have to match |
| LOD selection on the GPU (grass, terrain window) | compute pass writing per-level instance counts + `draw_indexed_indirect` | far trees are done — item (10) — and its shape is the template: a static placement table, one dispatch, indirect counts. Grass and the terrain window still rebuild their per-level lists on the CPU, and the terrain window's LOD is load-bearing for block residency, so it needs its own CPU reference first |
| model animation/compaction on the GPU | storage-buffer palettes, compute compaction | poses already reach the GPU, but activation/attachments/owners are the client's own gameplay callbacks, not render state |
| water batches on the GPU | the same machinery | the batch list is a handful of planes; there is nothing measurable to win |

`tree-lod-gpu-check` is that comparison for item (10): the compute pass's
per-batch counts and its `(fraction, mode)` records are read back and compared
against `tree_lod::select`, and every non-transition frame is rendered from both
paths and compared pixel for pixel. The two fade fractions are compared with a
tolerance instead, because f32 and f64 distances put the dither threshold on a
knife edge for a handful of pixels.

Item (10) is the first piece of that programme and shows the shape it takes:
nothing moves until the CPU's answer can be compared with the GPU's for the
same frame, which for LOD selection means reading the indirect counts and the
instance records back (`tree-lod-gpu-check`) rather than diffing pixels — the
pixels agree by construction once the selection does.

The gate is validation, not code volume. The only GPU this project can run in CI
is lavapipe, and there is no frame-time harness: a GPU-culled frame that is
wrong does not crash, it draws the wrong picture. So the honest order is to
extend the existing `scenery-pass-gpu-check` / `terrain-view-gpu-check` harnesses
so a compute-culled frame can be compared pixel-for-pixel against the CPU-culled
frame, and only then move one subsystem per change. That is a multi-change
programme, not a patch, and it is the reason this PR stops at folding the
commands rather than emitting them from a compute pass.

## 6. Validation

Everything above is covered by the existing CI: `cargo check --all-targets`, the
requirement-status and GFX-light audits, the viewer unit tests (now including the
slider-marker, cache-alias, flare-anchor, scenery-fold and indirect-record tests), the
bounded terrain RAM residency check, the Phase 1 shader regressions, the terrain
per-view GPU check, and the Windows x64 build.

The two GPU-driven subsystems are the first entries in this document whose
correctness cannot be argued from the code alone, so both have a check that
compares implementations rather than asserting a picture:

* `tree-lod-gpu-check` compares the compute pass's counts and `(fraction, mode)`
  records against `tree_lod::select` for every frame, and renders every
  non-transition frame from both the CPU packer and the compute pass, asserting
  the pixels are identical. Its adapter asks for the single-call draw path; the
  tree mip checks keep feature-free devices, so the per-batch fallback stays
  covered too.
* `grass-instance-gpu-check` renders the per-batch path, the single-call path
  and the CPU-expanded reference for every clip/fog combination and asserts all
  three frames are identical.

Both checks also post which draw path their adapter took as a `::notice::`
annotation, so a CI run says whether the fast path was actually exercised
rather than leaving it to the reader to assume.

The flare correction is a screen-space shift of an existing overlay, so the only
frame that changes is one with the sun on screen; the geometry itself (the 15
sprites, their sizes, their UVs, the `t` ladder and the sun→centre line) is
untouched, which is why the existing size/placement tests still apply.
