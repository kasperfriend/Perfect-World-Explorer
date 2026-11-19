# PART 3 — items 72–85, survey-then-relax

Each item is the same shape: a constant or equality in the reader stands in for
a range the format allows, so **survey the shipped data, support what the survey
finds, and make everything else a named skip** rather than a silent rejection.

Status is only `done` when the change is verified against a real archive. An
item that needs data which is not in this checkout says exactly what is missing;
it does not say "not established".

Corpus staged in this checkout on 18 November (all `google_drive/`, gitignored):
`configs.pck`, `gfx.pck`, `grasses.pck`, `textures.pck`, `trees.pck`, plus — via
the Drive connector — real `elements.data` from three clients, four `.t2lm`
lightmap files, eight `.ecbsd` placement files and `speedtreert.dll`.

Two earlier entries in this file were wrong and are corrected where they stand:
the claim that "no staged pack contains an EC-BSD" was true about packs and
false about the format (EC-BSDs live under `maps/world/`), and the T2LM tile
edge was not 256 for every file. Both are the same mistake — a survey answers
about the place it was pointed at, so each row now records *where* it measured.

## Done

| # | item | what changed | how it was verified |
| --- | --- | --- | --- |
| 76 | `angelica_pck` kept a second copy of the retail revision table | `Revision`/`REVISIONS`/guard pair are `pub(crate)` in `pck`, `angelica_revision()` maps `0x00020001`/`0x00020002` onto the `0x00020002` row, and the Angelica reader takes length, guard offset and count from it | real `google_drive/configs.pck`: 58 entries, first payload decodes to the 286 bytes its record claims; a test asserts the two tables are the same, so a later edit to `REVISIONS` cannot leave `angelica_pck` behind |
| 77 | `flags_at & 0x80000000` refused as a bare string | `ENCRYPTED_FLAG`, `ENCRYPTED_REFUSAL` and `is_encrypted_refusal()`; the refusal names the revision and the `dwFlags` word. `pck_lib` raises `EncryptedPackage`; `archive-census.py` prints `SKIPPED:` and exits 0 | a copy of the real `configs.pck` with bit 31 set: both readers produce the same named refusal, the census reports a skip, and the unmodified pack still reads 58 entries |
| 82 | 28 tests silently `return`ed over an absent staged client | `pck::corpus_file_absent` counts and prints the skip and fails when `PWE_CORPUS=require`; all 28 sites converted. The offline harness's `pck` stub was a silent no-op too and now counts | the skip prints and increments; the `require` path panics; two tests pin both |
| 73 | `KEYPOINTCOUNT == "1"` stood in for a count | `common()` split from `keypoint()`, count read from the file (bounded at 8), every keypoint kept, `Definition` carries `keypoint_count`/`keypoint_index`. `InterpolateMode != 0` and an out-of-range count remain **named** refusals | see the correction below: **per element, all 488 light elements declare `KEYPOINTCOUNT: 1`**, so the parse/reject split over the 466 light-bearing graphs is 466/466 before *and* after. This is a correct-shape change, not a corpus win, and the code comments say so |
| 74 | DDS FourCC match `DXT1`/`DXT3`/`DXT5` only; no `DX10` | `tools/dds-survey.py` added; `Uncompressed` gained `luminance`; 8- and 16-bit `DDPF_LUMINANCE` parse and `model_texture_gpu` widens the channel into RGB | 1,433 DDS members across the four packs went from **59 refused to 0 refused**; the 59 were the whole `textures/water/caustic/` set, which drew as the white placeholder |
| 75 | `terrain_gpu.rs` required DXT1 / one mip / **256×256**; `terrain_lightmaps::Tiles::parse` asserted `u(12) == 256` | the 256 was **wrong for three of the four staged files** and refused them outright: the dword at 12 is the tile edge, and 128 and 256 both ship. `tile_side` now reads it, accepts 128 or 256, and refuses anything else *with the number it saw*; `Tiles::side()` publishes it, `side_of` probes a header so the atlas can be sized before any payload is read. `terrain_gpu` sizes the atlas from that probe instead of pinning 256, requires the DDS edge to equal the header edge, and no longer compares the RGBA resize against the literal 256. DXT1 and one-mip were correct and stay enforced: no other format or mip count exists in the corpus | `tools/lightmap-survey.py` over 4 real files: `login_login`, `a91_6`, `a85_6` are 128 (8,320 B tiles) and `world_8` is 256 (32,896 B), all DXT1 ×16, all mips 0, header edge and DDS dimensions agree everywhere. The real reader accepts all four (`side=128` ×3, `side=256` ×1) where before it refused three. `terrain_lightmaps` 10 tests; `tools/test_lightmap_survey.py` 4 tests, wired into CI |
| 78 | `grass.rs` requires `nf == 1` and caps `nv>100_000`/`ni>300_000`; EC-BSD `n > 10000` | MOXB half: max `nv` 56 and `ni` 144 over all 138 meshes, `nf` exactly 1, so neither cap nor the equality is reachable — the caps are now named constants and `nf != 1` is split out as a *layout claim* with its own message. EC-BSD half: the earlier "no staged pack contains one" was **true about packs and false about the format** — EC-BSDs live under `maps/world/`, not in a `.pck`. Measured maximum is **88** names (`world.ecbsd`), so `ECBSD_NAME_LIMIT` sits 9,912 above it. Every refusal is by name; none is silent | `tools/ecbsd-survey.py` over 8 real files: revisions 17 (×1) and 18 (×7), name counts 1, 1, 1, 4, 4, 9, 12, **88**; all 8 parse completely under the three `water::path_string` rules, 0 refused. The names are `BSData/1.dat`…`BSData/88.dat`, i.e. grass-placement blocks rather than textures. `grass-check` 91 tests, including one pinning the limit above 88 for revisions 17 and 18; `tools/test_ecbsd_survey.py` 6 tests, wired into CI |
| 79 | caps at `scene.rs`, `pck.rs`, `water.rs`, `tree_source.rs`, `grass.rs`, `angelica_pck.rs` | every cap is now a named constant with its measured maximum beside it, and each refusal names the number it saw and the limit: `pck::ENTRY_LIMIT` 1,000,000 (largest staged pack `gfx.pck`, 3,499 entries; `angelica_pck` shares it instead of keeping a copy), `grass::MAX_MOXB_VERTICES` 100,000 and `MAX_MOXB_INDICES` 300,000 (measured over all 138 MOXB meshes: max 56 / 144), `grass::ECBSD_NAME_LIMIT` 10,000, `tree_source::TREE_SOURCE_LIMIT` 10,000 (measured max 59). `nf != 1` was split out of the MOXB bound and kept an *equality* with its own message, because it is a layout claim and not a bound | `grass-check` 90 tests; offline `bins` 39/39, which compiles `grass.rs` (the `check` module set does not, and caught an `E0308` this change introduced). `scene.rs` and `water.rs` already carried their maxima (22,528 blocks / 59 tree types / 244 grass types; 50 columns / 32 rows / 32 m cell) |
| 83 | `designer/catalog.rs` family `0x3000` | no change needed: the family word is `0x3000` in every sampled client, so the guard is right and only the *version* varies. `tools/version-gates.py` gained an `elements/task data` rule (dword `@0`, split as `(family << 16) \| version`) and now prints the family separately, because printing the raw word makes a newer client's v70 look like an unknown format rather than a newer revision of a family the reader already handles | four files from three clients: `0x3000000a` (v10), `0x3000000c` (v12), `0x3000000a` again, and `0x30000046` (**v70**, a much newer client). All family `0x3000`. Versions 10..70 against `ELEMENTS_V27 = 0x3000_001b` (v27), which is what `unsupported_layout` already reports by name and version |
| 80 | legacy 59-type SpeedTree bridge in `windows_setup.rs` | no change needed: the variable-count bridge is tried **first** with the type count as a fourth argument, and the 59-type path is a fallback taken only when that bridge rejects the argument. Above 59 types the fallback refuses by name and tells the user to rebuild the tool. The `59` is the legacy *protocol* size, not a viewer limit: `tree_source::TREE_SOURCE_LIMIT` is 10,000 | `tools/speedtree/bake.cpp` — the shipped `speedtree-bake.exe` already parses an optional third argument (default 59) and caps it at 10,000, which is the same bound `TREE_SOURCE_LIMIT` uses. The real `speedtreert.dll` (450,560 B, staged) is a genuine SpeedTreeRT build: it exports `CSpeedTreeRT`, `LoadTree`, `SaveTree`, `GetTreePosition`, `GetTreeSize`. Item 81's Windows build is what would exercise the fallback end to end |
| 84 | `startup_notice` should name the failed map and its error | no change needed: `world_load::load_with_main_world_fallback_notified` already builds `"Could not load map {preferred}; returned to the main world.\n\n{first_error}"`, so the notice names both the map and its error, and the both-attempts-failed path names both maps and both errors | `world_load` suite, 8 tests: `selected_map_error_returns_to_main_world_with_the_reason` asserts the notice contains `a01` *and* `bad instance terrain`; `notified_retry_receives_the_first_failure_reason` asserts the fallback attempt receives the first failure; `both_attempts_failing_reports_both_reasons` covers the double failure |
| 72 | `WATER_VERSIONS` is `[0x10002000, 0x10003000]` | no change needed: the constant is already a range-of-two with its measured corpus documented at the declaration, and `parse_record` falls back to a version-agnostic scan so an unknown revision still yields areas instead of being dropped | the comment beside the constant names the six clients surveyed and which word each writes. Nothing to relax: the one thing that *was* a hard rejection is already a fallback |

### The item-73 correction

The first reading of this item counted `KEYPOINTCOUNT` across a **whole graph**
and found up to 7, in 421 of 2,447 graphs, which made the `== "1"` test look
expensive. Counting **per element** -- which is the unit `decode` reads -- gives
a different answer: 463 of the 466 light-bearing graphs do contain a
`KEYPOINTCOUNT` above 1, but every one of them belongs to a sprite or particle,
and all 488 light elements declare exactly 1. The guard was therefore rejecting
nothing. Recorded here because the wrong number would have justified a much
larger change than the data supports.

## Blocked — needs data this checkout does not have

| # | item | what blocks it |
| --- | --- | --- |
| 81 | rodio features in `Cargo.toml` | Needs a Windows build to test the audio backend selection; the offline toolchain has no rodio |
| 85 | split the PR into four | **Plan written, not executed.** `Findings/PR-50-split-plan.md` partitions all **108** files (the item's 61 is stale) into four cuts and machine-checks the partition: 108 changed, 108 assigned, no orphan, no duplicate — A 20, B 8, C 23, D 57. Two things block execution: this session may only push to `arena/3074ae8a-perfect-world-explorer`, and the 110 commits interleave the cuts, so they are partitions of the final tree rather than disjoint slices — producing them needs four check-out/repair/verify cycles before any of the four PRs is worth opening |

### The corpus this depends on

The packs are reachable through the Google Drive connector and, once downloaded,
land in the gitignored `google_drive/` folder. `configs.pck` (218 KB),
`gfx.pck` (13.7 MB), `grasses.pck` (9.0 MB), `textures.pck` (82.7 MB) and
`trees.pck` (19.1 MB) are staged this way. The per-client folders are not in the
Drive listing this checkout was given, which is what blocks 75, 78, 79, 80, 83.
