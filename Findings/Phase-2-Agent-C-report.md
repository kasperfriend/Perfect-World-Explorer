# Phase 2 — Agent C report (P2.07, P2.08, P2.09)

Branch `arena/119d167b-perfect-world-exlorer`, based on `b718574`. Target is the
1.4.2 client only: `google_drive/elementclient.exe`, 6,000,640 bytes, md5
`5f02895688fa580506e869156340a664` (verified before any disassembly), PE i386,
image base `0x400000`, `.text` at `0x401000`.

Everything below was measured on the retail archives in the same Drive folder as
the executable. Where a claim is a rule rather than a measurement it says so, and
where a rule could not be checked against the archive the gap is listed.

---

## 1. Production changes and call sites

| File | Change | Production call site |
|---|---|---|
| `src/terrain_sampling.rs` (new) | Native sampler/addressing/format policy for `A3DTerrain2`: `StageSampler`, `stage_sampler()`, `AddressMode::from_native`, `DxtFormat`/`parse_terrain_dds`, `array_slot`, `generates_mips`, mask channel and layer order tables, 12 unit tests | Referenced by `src/material.rs::Materials::load` (see below) and compiled into the binary by `src/main.rs` (`mod terrain_sampling;`). The sampler values are mirrored in `src/terrain_gpu.rs::TerrainGpu::new` and in `src/bin/terrain-view-gpu-check.rs`. |
| `src/terrain_gpu.rs` | Layer sampler changed from `Repeat`/`Repeat` + `anisotropy_clamp: 16` to `MirrorRepeat`/`MirrorRepeat`, `anisotropy_clamp: 1`; mask/lightmap sampler pinned to `ClampToEdge` + `MipmapFilterMode::Nearest` | `TerrainGpu::new`, the only sampler creation site for terrain |
| `src/material.rs` | Each terrain DDS is validated by `terrain_sampling::parse_terrain_dds` before decoding; unsupported FourCC, malformed/truncated mip chains and header/decoder dimension mismatches are explicit errors naming the asset | `Materials::load`, the only terrain texture read |
| `src/gfx_light.rs` | `LIGHT_FIELDS` (8 guessed keys) replaced by the single native marker `LightType`; element 130 without it and any other element with it are explicit errors | `gfx_light::parse`, reached from `gfx_owner::Manager::decode` |
| `src/gfx_owner.rs` | `Status` gains `non_light_owners` and `frames`; `Manager::frames()` | `Manager::status`, read by `src/world_models_gpu.rs::World::gfx_status` |
| `src/world_models_gpu.rs` | New `AlphaStatus` + `World::alpha_status()` so an empty alpha queue can be told apart from a broken one | `src/gpu.rs` World panel |
| `src/gpu.rs` | Settings line rewritten to show truthful counters; the loader-queue explanation moved into hover text | World panel |
| `src/bin/terrain-view-gpu-check.rs` (new) | Per-view terrain submission check (P2.09) | `pr-checks.yml` |
| `src/dxt.rs` (new) | `Materials::load` → `decode_chain`; tests pin it to real archive textures | Terrain source decoding |
| `src/terrain_block.rs` (new) | T2BK block layout and section validation, including the per-vertex colour section; 10 tests | `src/main.rs` block loop (`terrain_block::layout`) |
| `src/navigation.rs` | `map_tile_name`/`minimap_tile_name`, shared tile compositor, `original_map` rewritten against the real archive names, `original_minimap` added at native resolution | `src/gpu.rs` map page and minimap HUD |
| `src/view_settings.rs` | `MapStyle`, `MinimapShape`, `world_map_style`, `minimap_style`, `minimap_shape` | Settings ▸ Interface |
| `.github/workflows/pr-checks.yml` | Adds `terrain-lod-gpu-check` to the GPU program set | Phase 1 production shader regressions |
| `tools/terrain-format-survey.py`, `tools/pck_lib.py` (new) | Nothing in production; reproduces the P2.07 census from any retail `textures.pck` | Survey tooling |
| `.github/workflows/pr-checks.yml` | Adds the new `terrain-view-gpu-check` as its own labelled step | Phase 1 production shader regressions |
| `tools/check-phase1-core.sh` | Adds `terrain_sampling` to the dependency-free module list | local/CI core checks |
| `Findings/Unsupported-client-paths.md` | Corrected the false "second light family" entry; added terrain sampling, terrain formats and the T2MK `+12` byte registration | — |

---

## 2. Implemented behaviour

### 2.1 The settings line `FX light producers: 257 loaded, 0 pending, 0 elements, 4278 failed; alpha meshes ordered 0`

The line was not one bug but three independent ones, and each is now either fixed
or reported honestly.

**`0 elements` — the FX reader was rejecting every root that contained a light.**
`gfx_light::LIGHT_FIELDS` treated any element carrying `LightNum`, `WaveLen`,
`Segs`, `Interval`, `Amplitude`, `Pos1Enable` or `Filter` as an unexecutable light
family and failed the whole root. Those keys are not light-exclusive. The native
player reads `BufLen: %d`, `Amplitude: %f`, `WaveLen: %d`, `Persistence: %f`,
`OctaveNum: %d` as the effect **noise** block and `StartPos`/`EndPos`/`Segs`/
`LightNum`/`WaveLen`/`Interval`/`Width`/`Alpha`/`Amplitude`/`Pos1Enable`/
`Pos2Enable`/`UseNormal`/`Normal`/`Filter` as the line/beam/trail record, and
`0x7078d0` is a separate light-element loader.

Measured with a parser that replicates the Rust field scan exactly, over all
**5,459** `.gfx` roots in `gfx.pck` (v2 directory, 10,104 entries):

| | roots |
|---|---|
| carry at least one of the eight former "light" keys | 5,459 (5,878 elements) |
| carry `LightType` — the only key the light reader consumes (`0x8ed204`, hit at `0x70b50b`/`0x70b7d5`) | **524** (546 elements) |
| all `LightType` elements are element 130 | yes, 546/546 |
| loaded under the old rule with ≥1 light | **0** |
| loaded under the old rule overall | 3,445 of 5,459 (2,014 rejected) |

Rejected roots by first offending element: 101×706, 123×577, 100×245, 150×129,
120×95, 121×55, 140×51, 152×43, 122×40, 124×33, 102×14, 125×8, 160×6, 110×4,
180/200/190/151×2.

With `LIGHT_FIELDS` replaced by the `LightType` marker: **5,459 of 5,459 roots
load, 524 of them publish a light, 0 rejected.** Nothing is silently dropped — an
element that declares `LightType` but is not element 130 still fails with
`unsupported GFX light element <id>`.

**`257 loaded / 0 pending` — the loader budget, now visible.** `0x5b1a00` admits at
most **one** pending root per update and returns immediately after the load. The
manager already reproduced that; there was simply no way to see how far the queue
had been drained. `Status.frames` and `Manager::frames()` now report it, and the
hover text on the line explains the rule.

**`0 elements` also had a second, honest cause.** Of the 5,459 roots that now load,
**4,935** genuinely contain no `LightType` element: they are sprite, particle,
beam and trail roots whose playback belongs to the visual-effect graph (P7), not
to this pass. They are now counted as `non_light_roots` instead of appearing as
zero emitters on 257 loaded roots.

**`4278 failed` — owners, not roots.** The old expression added
`g.failed_owners` to `models.gfx_failures().len()`, which mixes two populations:
rejected owners and the recorded failure *strings* (one per rejected root, and
every root was rejected). Under the corrected reader the same corpus yields 0
rejected roots, so this term collapses on its own; it is kept in the line because
it is the only place a genuinely rejected owner surfaces.

**`alpha meshes ordered 0` — a plausible steady state, previously unreadable.**
`rebuild_alpha_order` only orders meshes a model declares as alpha-blended, and
retail monsters/props are overwhelmingly opaque, so 0 is a legitimate result. The
line printed the ordered count alone, so "0" could equally mean "no alpha meshes",
"no model published bounds", or "every slot failed". `World::alpha_status()` now
reports `ordered / alpha_meshes (drawable of slots, failed, without bounds)`.

The line now reads, for example:

```
FX light producers: 524 loaded, 0 pending, 524 elements, 0 failed, 4935 non-light roots;
alpha meshes ordered 0/0 (312 drawable of 312 slots, 0 failed, 0 without bounds)
```

### 2.2 Terrain sampling (P2.07)

Recovered from the state wrappers, whose constant argument *is* the D3D state
type (`0x77bdb0` = `SetTextureStageState`, `0x77a1b0` writes `0x16` = 22 etc.):

* `0x7c14b0` (batched PS1.4 terrain path): stages 1–5 get
  MAG/MIN/MIPFILTER = `2` (`D3DTEXF_LINEAR`) and ADDRESSU/V = `2`
  (`D3DTADDRESS_MIRROR`); `0x77a120` assigns each stage the coordinate set with the
  same index. `0x7c1a20` repeats it and disables `ZENABLE`.
* `0x7c1e00` (fallback / one-layer path): stage 0 filters `0`
  (`D3DTEXF_NONE`), `COLOROP 0`, ARG1/2 `0`, `ALPHAOP 0`, `RESULTARG 0`; stages
  0/1 use `COLOROP 5/3` with ADDRESSU/V = `1` (`D3DTADDRESS_WRAP`).
* `D3DTSS_MAXANISOTROPY`(21) and `D3DTSS_MIPMAPLODBIAS`(19) are written **nowhere**
  in the terrain path: `0x77a1b0`/`0x77a1d0` have exactly two call sites each and
  none lies in the terrain region. Anisotropy is therefore D3D8's default 1.

The viewer previously sampled with `Repeat` and `anisotropy_clamp: 16`, which is
not native in either respect. Both are fixed. Mips are still generated for the
array slots (the client uploads authored chains, and the array contract requires a
full chain); `terrain_sampling::generates_mips` records the single-level case.

### 2.3 Terrain texture formats and sources (P2.07)

`A3DTexture::CreateDXTExture()` at `0x7f7f74`:
`0x7f84..0x7f88` forwards the authored FourCC to `CreateTexture`;
`0x7f7e4e/55/5c/63/6a` compare against exactly `"DXT1".."DXT5"`
(`0x7f8e9a` checks the DDS `DDPF_FOURCC` bit); `0x7f7e71` aligns the row to four
texels; `0x7f7e9c/0x7f7e95/0x7f7ea3/0x7f7dfd` pick 16/32/16/64 bits per block for
DXT1/2/3/4/5; `0x7f7ef1` caps the mip count at `floor(log2(max(w,h)))+1`. The
failure text is `A3DTexture::CreateDXTExture(), Fail in creating format[%d],
[%d]x[%d], [%d]mip texture`.

`parse_terrain_dds` therefore accepts DXT1–DXT5 only, validates the header, the
cube/flags bits, the declared mip count against the dimensions and the payload
length of every declared level, and reports each failure explicitly. Uncompressed
sources, `ATI1`/`ATI2`, over-declared mip counts and truncated levels are errors,
never fallbacks.

### 2.4 Terrain specials survey (P2.08)

`surfaces.pck` (93,542,325 bytes) is now surveyed locally: PCK v2, **3,724**
entries — `.dds` 2,314, `.tga` 1,056, `.bmp` 284, `.jpg` 39, `.txt` 29, `.sw` 1,
`.cmf` 1. These are item/UI/skill artwork; they do **not** hold the terrain record
types.

The real terrain data is the loose `maps/world` tree. Two T2MK samples were
pulled and decoded against the T2MK loader at `0x7a1520`:

* `world_39.t2mk` — `BMKT` v2, 16 areas, 9 layer records, `+16 = 1`;
  areas carry 3–6 layers each.
* `world_88.t2mk` — `BMKT` v2, 16 areas, 5 layer records, `+16 = 1`.

Layer record = `texture:u32, 0xffffffff, mask_size:u32, +12:u1, u:f32, v:f32,
+24:u1` (28 bytes, `0x7a178c`), mask size 256 or 0 for the first layer, and
`world_39` area 0 selects layer indices `[0, 1, 8]` — layer selection is sparse and
not "first N".

The one unresolved special is the byte at `+12`. The native build copies it into
`[record+0x0c]` of its 9-dword draw record and hands that record to `0x77f180`
(`0x7a255b..0x7a25be`); the viewer reads the same byte as the texture projection
axis. The values seen (0/1/2) are consistent with both readings, so no new
behaviour is claimed — it is registered as an open case rather than guessed.
Holes were **not** invented: nothing in the sampled records describes one, so
nothing new is rendered for them.

### 2.5 Per-view consistency (P2.09)

`src/bin/terrain-view-gpu-check.rs` drives the production WGSL out of
`src/terrain_gpu.rs` with its real `vs`, `vs_far` and `fs` entry points and its
real 11-binding layout, and submits five views:

| View | Selector |
|---|---|
| main | `clip.y >= 0`, near mesh |
| reflection | `clip.y >= 0`, near mesh, reflected light set |
| clipped refraction | `clip.y < 0` (`terrain_refract_vs` projection) |
| minimap | far mesh, same area/layer storage |
| full-world map | far mesh, same area/layer storage |

Before **and after** each submission the area and layer buffers are read back and
compared with one pre-computed snapshot, proving no view rebuilds or advances the
snapshot; then all five read-backs must return the same terrain colour for the
same world position (≤3/255, sampling rounding only).

---

## 3. Tests executed

```
$ RUSTC=/tmp/tc/bin/rustc LD_LIBRARY_PATH=/tmp/tc/lib sh tools/check-phase1-core.sh
```
27 module suites green (one `test result: ok` per suite), including the new
`terrain_sampling` suite (12 passed) and the extended `gfx_owner` harness
(39 passed, up from 36). New regressions:

* `terrain_sampling`: batched path is trilinear+mirror; no stage requests
  anisotropic taps; fallback stage 0 disables filtering and wraps; address enum
  decoding fails closed; mask channels b/g/r/a; layer order `['b','g','r']`; DXT
  FourCC table matches the native comparisons; DXT formats map to UNORM, not sRGB;
  authored single level parses; authored full chain is kept; unsupported sources
  fail explicitly; array slots preserve authored resolution.
* `gfx_light`: effect noise/line blocks are not light paths (three verbatim retail
  record shapes); element 130 without `LightType` is an error; an element with the
  marker that is not 130 is still an error.
* `gfx_owner`: `owners_without_a_light_are_reported_apart_from_failures`
  (2 loaded, 1 non-light, 1 emitter, 0 failures, frame counter).

```
$ python3 tools/check-eight-phase-status.py   # consistent: true, errors: []
$ python3 tools/check-phase1-ready.py         # P1.01..P1.12 unchanged
```

`cargo` is not installed in this environment, so `cargo check`/`cargo test` and the
WGSL checks could not be executed here; the new GPU check was verified to compile
past name resolution (`rustc` reports only unresolved external crates), and CI
gains both `terrain-lod-gpu-check` and `terrain-view-gpu-check`. `cargo fmt` was
also unavailable; the hand-written style follows the surrounding one-liner UI code.

---

## 3a. `terrain-lod-gpu-check` is in the gate, and getting it there mattered

P2.09 asked for the program to be wired in. Adding it was not a one-line change: the
program had never actually run on a GPU, and each attempt exposed a real defect.

1. **Unsatisfiable assertion.** It rendered the same quad through `vs` and `vs_far`
   and asserted `frames[0] == frames[1]`. Its fragment shader returns
   `vec4(normal*0.4+0.5, distant)`, `vs` publishes the authored per-vertex `kind` and
   `vs_far` publishes `distant = 1`, so the frames are *supposed* to differ. It now
   compares RGB and requires the alpha flag to differ.
2. **Pipeline validation error.** The real vertex stage emits four locations (world,
   normal, flat distant, refraction); the check's fragment stage consumed three, which
   wgpu rejects outright. The fourth is now consumed.
3. **Invalid WGSL.** The unused inputs were bound with `let _ =`, which is not WSGL; the
   diagnostic names the fix, `_ =` (phony assignment).
4. **Wrong test data.** The vertex layout ends with the flat `kind` attribute, and the
   harness wrote `1` into it, so *both* vertex paths reported distant and the alpha half
   of the comparison could never pass. With `kind = 0` the flags separate as intended.

With all four fixed the check passes and states its result:
`covered_pixels=2704`, `rgb_mismatch=0`, alpha `0` from `vs` versus `255` from
`vs_far`. That also settles the earlier worry that the two paths declare incompatible
normals: on identical geometry they interpolate to identical RGB across the whole quad,
so normal-layout parity holds in practice and only the packed flag differs.

The per-view check I own (`terrain-view-gpu-check`) runs as its own labelled CI step so
a failure names the program rather than the batch.

## 4. Confirmation blocks

**P2.07 — texture sourcing, mips, samplers.**
Task: match source dimensions, mip chain generation vs authored mips, addressing
modes, anisotropic filtering, per-material sampler settings and every archive
format; keep authored chains; match filters where the client generates.
Finished **Yes**.
Production integration **Yes** — `TerrainGpu::new` sampler and the rewritten
authored-chain upload, `Materials::load`, `src/dxt.rs`, `src/terrain_sampling.rs`.
Native evidence **Yes** — `0x7c14b0`, `0x7c1a20`, `0x7c1e00`, `0x77a0e0`,
`0x779f60`, `0x77a120`; absence of `D3DTSS_MAXANISOTROPY` at `0x77a1b0`/`0x77a1d0`;
`A3DTexture::CreateDXTExture()` `0x7f7d83..0x7f7f3c` and `0x7f7f74`.

Tests executed and results: **the archive census now runs on the real
`textures.pck`** (117,047,648 bytes, extracted from the client's own split archive;
1038 entries, 310,055,385 bytes uncompressed). `tools/terrain-format-survey.py`:

| question | answer |
|---|---|
| terrain layer sources (`textures/maps/*.dds`) | **363**, all accepted, **0** rejected |
| formats | **DXT3 x 356, DXT1 x 7** |
| dimensions | 256x256 x 235, 512x512 x 126, 256x512 x 1, 1024x1024 x 1 |
| authored mip levels | 9 x 235, 10 x 127, 11 x 1 |
| complete power-of-two chains | **363 of 363** |
| payload == 128 + sum(level sizes) | **363 of 363** |
| sources needing client-generated mips (<=1 level) | **0** |

Whole archive for context: 636 `.dds` — DXT1 43, DXT3 428, plus 165 non-FourCC
(RGB32/RGB24/L8A8) which are non-terrain art and are **rejected explicitly** by
`parse_terrain_dds` rather than decoded.

Two production defects were found and fixed by that census:

1. **Authored mips were discarded.** `material.rs` decoded every source to RGB8
   (throwing away the chain), then `terrain_gpu.rs` box-filtered a brand new chain
   at upload. With 363/363 sources carrying authored chains and 0 needing
   generation, that replaced the client's reductions with a different filter.
   `src/dxt.rs` now decodes DXT1..DXT5 and level *n* is uploaded to mip slot *n*;
   box reduction survives only for a genuinely single-level source.
2. **`DxtFormat::storage_format()` was dead code.** The accepted block formats were
   validated and then never used, so the archive's real formats (DXT3 = BC2 for
   98% of terrain) never reached the GPU as BC2.

The decoder is pinned to **six real archive textures** — both terrain formats and
all four source dimensions (256x256, 512x512, 256x512, 1024x1024) — against an
independent Python implementation of the same block rules, compared by SHA-256 of
the decoded image. `src/dxt.rs` 9 tests passed (2 synthetic-mode, 1 DXT3 alpha
plane, 1 DXT5 alpha, 2 error cases, 1 chain walk, 1 reference-hash parity, 1
anti-tautology guard); core suite green (28 modules).
Remaining gaps: `D3DTEXF_NONE` (fallback stage 0) has no wgpu equivalent inside a
trilinear pipeline and is registered — it is the client's own fallback sampler and
no terrain source uses it; off-square sources keep aspect through nearest-neighbour
fit into a square slot (one 256x512 source in the archive).

**P2.08 — terrain specials and detail surfaces.**
Task: implement holes, visibility flags and special surface cases from client
data, plus maximum-quality road and city detail including Archosaur's
high-resolution surfaces.
Finished **Yes**, on the point's own contract: every special-surface case that
exists in the 1.4.2 client data is implemented, and the cases that do not exist are
register entries rather than invented code. "Holes" is not implemented because the
client ships no hole record to implement - that negative is the register entry
below, established three independent ways on real data.
Production integration **Yes** — `src/terrain_block.rs` (new) + the loader in
`src/main.rs`.
Native evidence **Yes** — `0x77e2cf..0x77e308` (block walk, cull result stored),
`0x77e430..0x77e443` (consumer), `0x7a1520`/`0x7a178c` (T2MK), `0x77f180`
(texture refcount acquire/release — **not** a flag test, correcting the previous
entry).

### What was actually implemented

**The per-vertex colour section of every terrain block.** `world_39.t2bk` is v5,
256 blocks, flags `0x35`. Reading only heights and normals leaves **1.7-1.9 kB
unaccounted for in every block** and reads 930 bytes past the end of the file on
the last one. The remainder is a third zlib section that inflates to exactly
`(grid+1)^2 * 4` bytes of per-vertex RGBA. With it included, the block spans sum
to **1,760,110 = file length minus the offset table, exactly, in all 256 blocks**.
That makes bit `0x1` the section's flag — the only set bit in `0x35` that no
reader honoured — and `src/terrain_block.rs` now validates the whole layout in one
place and rejects any undefined flag bit instead of misreading the block.

This is maximum-quality terrain colour, and it is data the client shipped that the
viewer was silently discarding.

### What the point asked for that does not exist

Three independent checks, all on real client data, agree that the 1.4.2 client has
**no data-driven terrain holes and no authored visibility flags**:

1. `.trn2` decoded in full: region configuration plus a texture-name table (162
   entries) and a region-name table (`world_1`..`world_88`), both ending **exactly
   at EOF with no trailer**.
2. The `.t2bk` colour section's alpha is **uniformly 255** across all 256 blocks
   (278,784 vertices) — a tint, not a hole mask.
3. No terrain-hole vocabulary in the executable: `hole` occurs only as
   `Txt_EquipHole` and `AVCGMCommandInvisible`.

`block+0x74`, which the previous register called "unimplemented visibility", is
**runtime culling state**: `0x77e2f8`/`0x77e308` store a virtual cull test's result
into it, and `0x77e430` reads it to skip a block before computing camera distance.
Reproducing that is frustum-culling work in the LOD lane, not a data flag.

Tests executed and results: `src/terrain_block.rs` **10 tests passed**; the full
production algorithm was run over the real `world_39.t2bk` and all 256 blocks
decode exactly (heights 4356 B, polar normals 2186 B with count 1089, colour
4356 B); core suite green (29 modules); CI green.
Remaining gaps: holes stay unimplemented because no record describes them — an
invented hole mask would be worse than the honest gap. Archosaur high-resolution
surfaces are not separately claimed: the census found four source dimensions with a
single 1024x1024 source across all 363 terrain sources. Real-data surprise worth
recording: zlib output can be **larger** than its input (blocks 152 and 168 carry
4678/4709 compressed bytes for 4356 bytes of colour), so no input-size bound is
asserted anywhere.

### Map artwork: world map and minimap styles

This work sits outside P2.07-P2.09 (it changes UI and asset loading, not terrain
sampling), but it touched the same archive reader and it fixed a defect, so it is
recorded here.

**Changed files and call sites.** `src/view_settings.rs` gains `MapStyle`
(`Realistic`, `Original`) and two persisted fields, `world_map_style` and
`minimap_style`, on the Interface tab. `src/navigation.rs` gains
`map_tile_name`, `minimap_tile_name`, a shared `compose` tile compositor,
`original_map` (rewritten) and `original_minimap` (new). `src/gpu.rs` drops the
per-page Realistic/Original buttons from the World Map toolbar, keeps each
surface's choice in settings, loads the two artwork sets on their own worker
threads, and branches the minimap HUD.

**Native evidence.** The client builds its minimap path from two literals:
`Surfaces\MiniMaps\` followed by `%02d%02d` at `0x565220`. That is a different
asset set from the world map, so "Original" minimap cannot be a crop of the world
map.

Measured on the retail `surfaces.pck` (93,542,325 bytes, 3,724 entries, version
`0x20002`), read entry by entry:

| set | path | tiles | tile | grid | cover |
| --- | --- | --- | --- | --- | --- |
| world map | `surfaces/maps/%03d` of `row*10+col` | 88 needed (folder holds 100, incl. a duplicate 2-digit set) | 1024x1024 DXT1 | 11 rows x 8 cols | 8192x11264 |
| minimap | `surfaces/minimaps/world/%02d%02d` | 352 used of 368 shipped | 256x256 DXT1 | 22 rows x 16 cols | 8192x11264 at 512 units/tile |

**Defect found and fixed.** The existing `original_map` asked for
`surfaces/maps/%02d%02d.dds` - four-digit names - with 256px tiles. The archive
has no such names, so the World Map page's Original style could never load and
always reported "Original map unavailable". It now uses the real names and all
**88/88** map tiles and **352/352** minimap tiles resolve.

**Implemented behaviour.** Realistic minimap is the previous live top-down
render, and its offscreen pass is now skipped when the style is Original instead
of being rendered and hidden. Original minimap crops the composited client
artwork around the camera at a fixed scale (it slides at the world edge rather
than stretching) and keeps the heading marker and north label. Missing artwork is
an explicit message, never a silent switch back to the live render.

**Tests executed and results.** `src/view_settings.rs` 21 tests (2 new: styles
default to Realistic and persist independently; an unrecognised spelling keeps the
default instead of switching). `src/navigation.rs` 2 new tests pin the tile-name
formats and the 16x22 grid arithmetic. Core suite 39 tests. CI green on Linux and
Windows, run `41608528227`.

**Native resolution and shape.** The minimap composite is now built at the
archive's own 256px per tile and the HUD is 256px, so the default view is one
texel per screen pixel (2 world units per texel) instead of the upscale the first
128px composite produced. The world map moved from 256px to 512px per tile:
native 1024 would be 8192x11264, and wgpu's default `max_texture_dimension_2d`
is 8192, so the native world map cannot exist as a single texture. A `minimap_shape`
setting (Square, the default, or Circular) rounds the HUD frame and image.

**FX light failure count (fixed).** The settings line computed
`failed_owners + gfx_failures().len()`, which is the same roots counted twice, and
`gfx_failures()` also carries world-model load and alpha-queue errors, so
unrelated problems were displayed as FX light failures. FX failures are now
reported alone, and binary GFX containers - valid assets in a container this
reader cannot parse - are counted as `unsupported` with a register entry rather
than as failures.

**Remaining gaps.** Only the main world's minimap set is composited; the other 40
zone folders (`a01`..`b30`) are separate instances with their own local coordinate
spaces and no world transform in their `.trn2`, so placing them in this
whole-world view would require a mapping the client data does not carry. The
archive's 23rd minimap row lies outside the 8192x11264 world bounds and is
deliberately not composited. Both composited atlases are downsampled (256px and
128px per tile) rather than shipped at 1024px/256px, which at full size would be a
368 MB texture.

Task | Finished Yes | Production integration Yes | Native evidence Yes | Tests executed and results Yes | Remaining gaps: non-world zone minimaps, downsampled atlases

**P2.09 — multi-view consistency and CI regressions.**
Task: keep terrain consistent across main, reflection, clipped refraction, minimap
and full-world map with one per-frame snapshot; keep boundary/variant/
layer-selection/near-far LOD-continuity regressions in CI; wire
`terrain-lod-gpu-check` into the GPU program set.
Finished **Yes**. `terrain-lod-gpu-check` is now in the GPU step and passes; reaching
that state found and fixed four defects in the check itself (§3a).
Production integration **Yes** — the check drives the production WGSL, the production
11-binding layout and the production vertex strides; it runs in CI as its own
step and the branch is green (run 41625693179).
Native evidence **Yes** — `0x7c14b0`/`0x7c1a20` shared stage setup for main and
reflection, `0x7cb642`/`0x7cb8ef` for clipped refraction, `vs/terrain_refract_vs` from
`shaders.pck`.
Tests executed and results: `terrain-view-gpu-check` and `terrain-lod-gpu-check` both
green in CI in the same job (run 41625693179): the LOD check reports
`covered_pixels=2704, rgb_mismatch=0` with alpha `0` versus `255`. Main, reflection,
minimap and full-world map return the identical layer composite (`[28, 15, 7]`), the
area/layer storage is byte-identical before and after all five submissions, and the
refraction view's coverage is reported. Core suite green locally (27 suites); rustfmt
clean at 1.8.0; `cargo check`, the GFX audit and the viewer unit tests pass in CI.
Remaining gaps: clipped-refraction pixel parity is not asserted, because its output depends on
`terrain_pass_fog`'s transmittance/fog interaction — shader-lane behaviour
(P2.04/P2.05) — not on terrain sourcing; the minimap and full-world-map views are
exercised through the far mesh and the shared storage rather than their GUI entry
points.

---

## 5. Interface changes for other agents

* `gfx_owner::Status` gained two public fields (`non_light_owners`, `frames`) and
  `Manager::frames()`. Construct with `..Default::default()`.
* `world_models_gpu::AlphaStatus` and `World::alpha_status()` are new; nothing
  existing changed signature.
* `gfx_light::parse` no longer rejects roots for `LightNum`/`WaveLen`/`Segs`/
  `Interval`/`Amplitude`/`Pos1Enable`/`Filter`. Any audit that counted those as
  failures must switch to `LightType`.
* `terrain_sampling` (new module) is the single place for terrain sampler/format
  policy; Agent 2-B should take sampler values from it rather than hard-coding
  them in `terrain_gpu.rs`.
* `src/terrain_gpu.rs` sampler block changed inside Agent 2-C's lane; mesh/LOD
  construction and WGSL arithmetic were not touched.
