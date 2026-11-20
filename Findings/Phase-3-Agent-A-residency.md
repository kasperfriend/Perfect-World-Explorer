# Phase 3 Agent A — texture residency measurement (P3.08)

All numbers below are **measured** by running the whole-world preload over the
staged 1.4.2 corpus, not estimated.

- Corpus: `maps/world/world.ecwld` + `world.ecbsd` (16,037 placements),
  `litmodels.pck` (28,858 BMD members), `building.pck` (5,391 texture members).
- Command: `./target/release/world-preload-check google_drive`
  (client exe md5 5f02895688fa580506e869156340a664 verified before use).
- Budget: 256 MiB default (`DEFAULT_TEX_BUDGET_MIB`), override with
  `PW_TEX_BUDGET_MIB`. Runtime: ~21 s for the full world.

## Decoded-texture store (CPU side, `Objects::TextureCache`)

Content-hash (SHA-256 truncated to 64 bits over width‖height‖RGBA) dedup +
byte-budgeted LRU eviction with batch pinning.

| counter | value |
|---|---|
| unique textures resolved from archives | 3,136 (`archive_hits`) |
| total decodes performed | 4,709 |
| re-decodes caused by eviction pressure | 1,573 (4,709 − 3,136) |
| content-dedup alias hits (distinct names, identical pixels) | 41 |
| resident bytes avoided by dedup | 9,357,548 |
| LRU evictions | 4,135 |
| bytes freed by eviction (cumulative) | 1,698,712,479 |
| **peak resident decoded bytes** | **272,167,807 (259.6 MiB)** |
| final resident decoded bytes | 268,361,694 (255.9 MiB) |
| budget | 268,435,456 (256 MiB) |
| batches where pinned textures alone exceeded budget | 0 |

Cumulative decode traffic was ~1.70 GiB while peak residency stayed at
259.6 MiB: the full world's full-res source textures are demonstrably never
all resident at once. Peak exceeds the 256 MiB budget by <4 MiB because a
store completes before its eviction pass (bounded by one texture + pinning);
the final figure is inside budget and `pinned_over_budget` never fired.

## GPU pages (whole-world preload mode)

Page atlases are 4096² RGBA with 128-px grid packing and 7 mips; each page is
trimmed to `used_height`. In preload mode pages are retained by design (the
whole world is the target); residency bounding applies to the decoded source
store above, and streaming mode (`Objects::update`) additionally drops models
outside the active batch.

Measured, same machine/data, before vs after content-keyed slot dedup:

| build | pages | atlas base | with mips | fallback-texture meshes |
|---|---|---|---|---|
| commit 8e2d005 (name-keyed slots) | 66 | 4,144.0 MiB | 5,525.2 MiB | 3 |
| this branch (content-keyed slots) | **65** | **4,090.0 MiB** | **5,453.3 MiB** | 3 |

Delta: −1 page, −54.0 MiB base, −71.9 MiB with mips, from 41 alias references
collapsing onto already-packed slots. Unit test
`world_objects::page_tests::content_dedup_reduces_page_turns` proves the
mechanism in isolation (128 alias references → 0 page turns; 65 unique
contents → 1 page turn). Eviction proof: `objects::tests::lru_eviction_keeps_residency_inside_the_byte_budget`
plus the 4,135 measured evictions above.

## Resolver census for this run (whole-world subset)

| counter | value |
|---|---|
| texture references queried | 39,416 |
| resolver cache hits (client-style name cache, 0x77c160) | 36,274 |
| archive hits | 3,136 |
| disk hits | 0 |
| missing (whole corpus: 2 distinct, 3 references) | 2 distinct |
| missing with client log suppression (`Textures`/`Tex_` substrings) | 0 |
| `.sdr.` shader-registry references | 4 distinct (45 queries), all file-backed |
| nrf-derived normal-map references | 39,416 (all → rf_default.dds fallback) |
| register rows emitted | 3,148 |

Note the two corpus-missing names (`building/textures/h/865.psd`,
`building/textures/s/16e0.dds`) contain lowercase `textures`; the client's
suppression check is `strstr` against capital-`Textures` (0x76d88d), so the
client would LOG these misses — reproduced literally (`missing_quiet = 0`).

## Limitations (documented, not silently skipped)

- `textures.pck` (117 MiB) could not be staged: the Drive download path
  refuses files >100 MiB and no split parts exist for it. It is not mounted,
  so any reference that would resolve there appears as a `missing` register
  row (the census found none: every BMD texture reference in the corpus
  resolves in building.pck or is one of the 2 known-missing names).
- `shaders.pck` is not staged, so all 39,416 nrf-derived normal-map
  references take the client's own `rf_default.dds` fallback path — which is
  also absent, exactly like the unstaged client install; the client tolerates
  this (A3DTexture::Load returns TRUE on open failure, 0x78a440). Rows are
  registered per distinct name (`nrf-default-missing`, 3,142 rows).
- `865.psd` is a Photoshop file; no staged decoder handles PSD. The reference
  is registered (`missing` row) and the 2 meshes fall back to the neutral
  swatch, matching the pre-existing renderer behavior.
- The 4 `.sdr.` names are resolved by the documented file-backed
  approximation (registry is runtime state, 0x785d30): their same-named
  `.sdr.tga` members in building.pck are decoded and every occurrence gets a
  `shader-registry-file-backed` register row.
