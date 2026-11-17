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
