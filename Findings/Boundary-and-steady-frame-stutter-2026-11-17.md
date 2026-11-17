# Boundary and steady-frame stutter: what the streaming path actually costs

Reported: flying fast across areas and tiles produces a moderate hitch at each
tile/area boundary, "as if something is loading", and looking around the world
produces a periodic stutter with the same feel. The reporter guessed music.

Nothing below is a measurement: this sandbox has no GPU and no frame-time
harness, so the analysis is a reading of the code paths that run on the render
thread, with the pre-existing diag channels named so the claims can be checked on
the machines that have a GPU. Every change here removes work or removes an
allocation; none of them changes what is drawn.

## Music is not the cause

`Zones::open` parses `precinct.clt` three times at startup and then once per
zone entry (`Zones::find`, a linear scan), and `DirectoryIndex::resolve_under`
does one cold `fs::read_dir` on the render thread when a track is first named in
a new zone. Both are milliseconds, both happen on a zone *entry* rather than on
a tile boundary, and the track decode plus the `rodio` playback are on the
`zone-music` worker (Windows only). That is a real cost, but it cannot explain a
hitch every time a block window slides, and it is not what was changed here.

## The boundary hitch (one block of travel)

`Renderer::update_mesh` runs on the render thread whenever the streamer hands back
a rebuilt world, and it used to re-upload the whole near world unconditionally:

1. **Grass atlas.** `build()` filled a fresh 4096² RGBA atlas, padded it, copied
   every nearby grass material into it, generated eight mip levels and the
   independent per-tile mip array, and `update_mesh` uploaded all of it. The
   packer is deterministic and the sources are archive members, so for the
   overwhelming majority of block crossings the new atlas is byte-identical to
   the one the GPU already holds. This is the single largest upload in the path.
2. **Streamed scenery pages.** Each `world_objects::Page` was rebuilt as a fresh
   64 MiB zeroed atlas per page, then uploaded: a 4096² texture, eight mip
   uploads and the page's vertex and index buffers. Most pages of a crossing are
   the pages that were already resident, with the same textures in the same
   rectangles.
3. **Near-world geometry.** The terrain vertex and index buffers were destroyed
   and recreated (`create_buffer_init`) for a mesh whose size changes by about
   one block.
4. **Grass field.** The four grass buffers (mesh, indices, every placement
   instance, indirect records) were likewise destroyed and recreated on every
   update.

Fixes, in the same order:

- `texture_atlas::LayoutKey` signs the atlas *layout* (texture name + rectangle,
  in packing order). `build()` accumulates it while packing, and the streaming
  request carries the signature the renderer currently holds, so `build()` skips
  the zero-fill, the pixel copies and the mip generation entirely when the layout
  did not move; `update_mesh` skips the upload on the same test. One root-owned
  key is the only new state.
- `local_page_key` signs each finished scenery page (sorted texture list with
  their rectangles, frozen geometry sizes, placement ranges, bounds, SDR flag).
  `update_mesh` keeps the `GpuPage` whose key it already has and uploads only the
  pages that really changed. Page atlases are now grown as the packer descends
  (`reserve_page_atlas_rows`) instead of allocating and zeroing 64 MiB up front.
- `Renderer::write_world_buffers` writes into capacity-rounded buffers and only
  reallocates when the mesh outgrows them.
- `grass_instances::Gpu::update` takes the queue and writes into its existing
  buffers, growing them to the next power of two when needed.

Two latent bugs were found next to this work and fixed with it:

- `world_objects::Page::finish` truncated its atlas to `SIDE × height` without
  growing it first. That was harmless while the packer allocated the full square
  and would have read past the end of a shorter buffer under the incremental
  grow; it now pads to the exact upload extent (and asserts nothing else).
- `GpuPage::upload` copies `SIDE × height` texels; the growth path keeps that
  exactly equal to the buffer `finish` hands over.

## The steady stutter (no travel at all)

`gfx_owner::Manager` holds every ECWLD effect record the map places — a few
thousand owners — and `World::update_pass` writes per-owner flags once per frame:

- `apply_activation` calls `set_active` for every producer, every frame;
- `apply_attenuation` calls `set_attenuation` for every attenuating producer,
  every frame;
- `resolve_attachments` calls `set_attachment` for every hooked producer.

Each of those resolved the identity with `owners.iter().find(...)`, so the flag
traffic was O(owners²) per frame: millions of 64-bit identity comparisons to
carry information that changes for a handful of owners. `Manager` now keeps an
`index: HashMap<u64, usize>`, the three setters resolve through it, and the index
is rebuilt (`reindex`) by the only two paths that remove owners plus `clear`.
`set_*` still re-checks the identity it found, so a stale index can only miss,
never dim somebody else's root.

The second steady cost was double root inflation. Root loading is budgeted to one
root per frame (`load_one`), but the animated half is built in the same update
(`build_one_player`), and it re-read, re-inflated and re-decoded the same archive
member the loader had just decoded. `Manager::last_root` keeps that one root's
text, so an owner admitted this update is parsed once instead of twice; the cache
is one entry and is keyed by path, so it cannot grow with the world.

## Checked and deliberately not changed

- `TerrainGpu`'s mask atlas and the far-world mesh are only rebuilt when the
  mask window changes (`t.terrain_material_window != Some(mask_window)`), not on
  every crossing, so the `poll(Wait)` calls in `upload_masks` are not a
  per-crossing cost.
- `device.poll(Wait)` in the texture-upload loop of `TerrainGpu::new` runs once
  at startup and bounds staging memory on shared-memory adapters; lowering its
  frequency trades a documented memory guard for speed, which is not this change.
- Effect and model shader pipelines are already cached by key
  (`gfx_effect_gpu::Draw::pipelines`); a new pipeline only appears with a new
  material/blend combination.
- `client_region_stream::Window::update` already budgets admissions per frame, so
  fast flight slides the window instead of admitting an unbounded batch.
- Music/ambience: see above. The zone-entry `fs::read_dir` remains a candidate
  for a later change, but it is not a boundary cost.

## How to confirm on a GPU

- `Renderer::update_mesh` now emits one `TimingLoad` (channel 32) line per
  update — scenery pages kept, pages uploaded, grass atlas kept or re-uploaded.
  Work avoided is directly countable, which is the claim made here; a line that
  keeps reporting uploads on a crossing would falsify it.
- `PWE_EFFECT_CENSUS=1` prints the per-root clocks, and the status line prints
  the playing count, so the loader/`last_root` path can be watched as effects
  stream in.
- `TimingFrame` (diag channel 31) and `TimingLoad` (32) already separate frame
  time from load time; the four upload sites above no longer appear in a frame
  that did not change the layout.
- The GPU regression route is the existing record/pixel harnesses
  (`tools/static-model-gpu-programs.txt`, `tree-lod-gpu-check` as the template):
  the keys are compared only between two builds of the same page layout, so the
  pictures must be identical, not merely similar.

## Files

- `src/texture_atlas.rs` — `LayoutKey` (FNV-1a over name + rectangle, in packing
  order) and its unit test.
- `src/gpu.rs` — `MeshData` carries the grass key and the per-page keys; `build`
  skips atlas materialisation when the layout is unchanged, grows page atlases
  incrementally, and signs each page; `Renderer` keeps the keys, reuses unchanged
  `GpuPage`s, and writes world geometry into capacity-rounded buffers.
- `src/grass_instances.rs` — `Gpu::update` writes into its buffers through the
  queue and grows them instead of recreating them.
- `src/world_objects.rs` — `Page::finish` pads the atlas to the exact upload
  extent; two unit tests cover the grown and the short buffer.
- `src/gfx_owner.rs` — identity index with `reindex`/`position_of`, the three
  setters through it, `last_root` reuse in `build_one_player`, two unit tests.
- `src/bin/grass-instance-gpu-check.rs` — the new `Gpu::update` signature.

## Second pass: fast flight with the world resident (same day)

The first pass fixed what a crossing uploaded. The follow-up report was that the
lag survives "flying fast over zones **with the world fully loaded to VRAM**" —
so the cost is not streaming at all, it is work the crossing and the frame do
regardless of how much is already resident. This pass re-walked the crossing
path, the per-frame render path and the whole-world preload handover, and found
four things, three of them work on data nothing samples.

### 1. The ten-class placement list was rebuilt per crossing

`ten_class_placements` walks every ECWLD block, then every tree, water cell and
ornament in the scene and produces a `Placement` per one of them — tens of
thousands of entries on a retail map — behind a `by_export` hash map it also
builds from scratch. It ran on every block-window crossing. Nothing in its output
depends on the camera, the window or the detail settings; the list covers the
whole map by construction. It is now built once and held on `Terrain`
(`scenery_placements`, an `Arc` so the streaming worker owns it and `build` can
still borrow other fields mutably), and a `TimingLoad` line records the entry and
block counts the one time it is built.

This matters more than the crossing cost suggests: `build` runs on the
`world-streaming` thread, so world-sized CPU work there competes with the render
thread on every crossing instead of being a one-off during load.

### 2. The local scenery half was rebuilt and uploaded while the whole world drew instead

`draw_opaque` already ends the local index range at `object_start` and switches
the pages over atomically (`world_done`). But the streamer did not know that: on
every crossing it still selected ornaments, packed their textures into 4096²
atlases and appended their geometry into the mesh, and `update_mesh` still
created a texture per page, wrote its eight mip levels and uploaded the grown
vertex/index buffers. All of it is skipped by the draw path:

```rust
let local_end = if self.world_done { self.object_start } else { self.water_start };
```

and `terrain_end == object_start` in every build (nothing is appended between the
two markers), so the range that would draw the local ornaments is empty while
`world_done` holds. The crossings were paying for tens of megabytes of atlas that
no pass samples.

The request now carries `skip_local_scenery` (the renderer's `world_done`), and
`build` leaves the ornament half out entirely: `nearby` comes back empty, and
every list below it — residency bits, atlas packing, page atlases, ornament
geometry — is built from `nearby`, so an empty one makes the block do no work at
all instead of needing a second code path. The flag travels back in `MeshData` and
is part of the staleness test, so turning the whole-world preload *off* rebuilds
with the local scenery again. That also closes a hole that predates this change:
unchecking "Render whole world" cleared `local_pages` and set `world_done` false
but changed nothing in the staleness test, so the objects stayed missing until the
camera happened to cross a block.

### 3. Console writes on the crossing path

`Terrain residency`, `Scenery residency`, `Local scenery` and the background
build time were `eprintln!`, i.e. a synchronous stderr write on the render thread
on every crossing (and the background build line from the worker). They are
`TimingLoad` lines now. `TimingFrame` (31) and `TimingLoad` (32) were also missing
from `Channel::wired`, which is what the Logging tab reads to decide whether a
channel can say anything — the tab marked both as having no call sites, so neither
could be turned on deliberately. Both are wired.

### 4. A per-second frame budget, because the next question is "which stage"

`TimingFrame` was described in the channel list ("Frame time per stage, once a
second") and had no call site. `Renderer::render` now times itself in stages —
`visibility`, `main scene`, `extra views` (the water mirror and both refraction
submits), `post`, `minimap`, and `rest` — and emits one averaged line a second
with the worst frame, the frames per second and the world page count. The minimap
pass, which runs outside `render`, adds its own stage. A frame that reads a few
milliseconds of CPU while the frame rate stays low is GPU-bound, and the line says
so by arithmetic rather than by guess.

That pass also had a real defect: it overwrote the *shared* indirect-argument
buffer with the map's terrain records, so the next frame's `queue_main_runs` always
saw a different record list than `prepare_runs` had compared against and
re-uploaded the main view's records. `render_minimap` now puts the main view's
records back (and does not have to when it drew the full map, which uses the whole
range).

### Still unproven, and what to measure

- The main view is recorded three times per frame: the scene, the water mirror and
  the above-water refraction each draw the terrain runs and the world pages. That
  is what a mirror costs; there is nothing to skip without changing what the water
  reflects, so nothing was changed.
- `world_models_gpu::draw_models` writes a uniform block per mesh per view, and
  `WaterEffect 0` (the shipped default) already gates EC models out of the mirror
  and the refraction, so in the default configuration it runs once per frame. A
  non-default water effect would pay for it three times; that is a real cost and a
  real lead, not a change this pass could make without a GPU to check it against.
- The realistic minimap is re-rendered on every presented frame while the camera
  moves, by design (the comment above the call explains that the old 350 ms timer
  capped it below 3 FPS). Its cost now shows up in the `minimap` stage, so the next report can
  either justify a rate cap or retire the idea.
- Nothing here is a measured frame-time win. This sandbox has no GPU: the claims
  above are about work removed from data no pass samples, and the counters that
  make them checkable (`TimingLoad`, `TimingFrame`) are the evidence a GPU run has
  to produce.

## Third pass: the minimap, and the whole per-frame ledger (same day)

### The minimap question, answered

**With the minimap style on Original, no top-down render happens at all.** The
only live minimap pass is behind `settings.show_minimap && settings.minimap_style
== MapStyle::Realistic` in the redraw handler, and the HUD draws the client
artwork (`surfaces/minimaps/world`) instead. The whole-world map texture is gated
the same way (`world_map_style == Realistic`) and only re-rendered every two
seconds while the world map is open. So Original is not paying for a hidden
realistic pass.

What *was* true, and is now fixed:

* The realistic minimap re-rendered whenever the camera's **height** changed. Its
  key carried `camera.pos.y.round()`, but `minimap_matrix(x, z, range)` never
  reads height — the map is an orthographic window centred on the ground
  position. Climbing, bobbing or pressing E re-rendered the entire top-down scene,
  every frame, for an image that could not change. The key is now a typed
  `MinimapKey` built from a ground position, so height has no way into it.
* The key did **not** carry `minimap_range`, which *is* in the matrix: dragging
  the range slider left the previous window on screen until the camera moved a
  whole unit.

The thing that feels like "the world drawn twice" is the water: a visible
non-simple water plane costs a mirror **and** a refraction render of the scene, so
one pond in view is three world renders per frame before the minimap adds a
fourth. That is the ledger below.

### The per-frame ledger

World renders (each one is sky + near terrain + far terrain + far trees + grass +
scenery pages + SDR replay, and, for the water views, clouds and weather):

| # | pass | when | cost driver |
|---|------|------|-------------|
| 1 | main scene | every frame | pages, models, grass |
| 2..n | water mirror + refraction | **two per visible non-simple water plane** | plane count × scene |
| +1 | realistic minimap (320²) | camera ground position or range changed | scene, window only |
| +1 | whole-world map (1024×1408) | world map open, Realistic, every 2 s | the entire world, unculled |

Non-render passes per frame: object-motion clear, world models (+ effect-motion),
add-ons, clouds, weather colour + weather motion, water `render_edge` per plane,
shoreline waves in their own encoder and submit, post chain, egui, and the
upscaler.

CPU per frame: the stage timings in the `TimingFrame` line, plus the per-instance
model loop and its per-mesh uniform writes, the alpha-order rebuild, the static
light-grid rebuild when the camera crosses a 12-unit cell, two scoped threads for
the terrain visibility walk, and the minimap's own matrix and run queueing.

### Fixed in this pass

1. The minimap key (above): height removed, range added.
2. `copy_scene_to_refraction` and `copy_depth` ran every frame. The refraction copy
   is read by the per-plane refraction renders and the water surface only, so it now
   follows "a non-simple water plane is visible"; the depth copy is
   `PostGpu::render`'s input alone, so it follows `use_post`. A land view no longer
   pays a full-frame texture copy.
3. `StaticGrid::update` compared the captured light corpus against every light on
   the map *before* the cheap 12-unit cell test. Flying crosses a cell most frames,
   so the comparison ran on exactly the frames where it was then thrown away. The
   cheap tests now come first; a stationary camera still detects a replaced corpus,
   which is what `replacing_static_corpus_invalidates_cells_without_camera_motion`
   pins.
4. `TimingFrame` now reports **world renders per frame** next to the stage
   milliseconds. A frame that renders the world four times is bound on the GPU in a
   way no CPU timing shows, and this is the number that says so.

### Found, and deliberately not changed

* **The water multiplicity.** A mirror is a mirror: skipping the reflection or the
  refraction for a distant plane, capping the plane count, or scissoring the pass to
  the water's screen rectangle all change pixels that exist today. The count is now
  measured instead of guessed at, so a future report can decide.
* `copy_scene_to_refraction` looks **redundant even when water is visible**: both
  refraction branches clear the whole target (fog colour when the camera is above
  the plane, sky + clear when it is below) before drawing, so the copy is
  overwritten before anything samples it. It is kept because proving that needs one
  A/B run on a GPU with water on screen, and a wrong call here is visible garbage.
  The guard added above is the part that is safe.
* The sky, far trees and grass are drawn in every `draw_opaque`, including the
  top-down map and the mirror. Removing any of them changes pixels at the map's
  edges or in the reflection.
* The two scoped threads that walk the terrain visibility lists (`std::thread::scope`
  in `render`) run per frame. At whole-world scale the far list is ~1k spans, so two
  spawns (~10–30 µs each) are roughly the cost of doing the walk inline; which way
  wins is a measurement, not a deduction, so it stays.
* `wgpu::TextureView`s are created per frame (`scene_depth` and the water accessors'
  views, ~8–12 objects). Caching them would tie the renderer to the target size for
  a few microseconds a frame.
* The shoreline pass submits its own command encoder. One submission per frame is
  the whole cost.
* Terrain mask atlas: `upload_masks` allocates a texture and calls
  `device.poll(Maintain::Wait)` — but only when the mask window changes, which needs
  `Original-game terrain cutoff` on. In the whole-world configuration it runs once.
