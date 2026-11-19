# PART 3 — items 72–85, survey-then-relax

Each item is the same shape: a constant or equality in the reader stands in for
a range the format allows, so **survey the shipped data, support what the survey
finds, and make everything else a named skip** rather than a silent rejection.

Status is only `done` when the change is verified against a real archive. An
item that needs data which is not in this checkout says exactly what is missing;
it does not say "not established".

Corpus staged in this checkout on 18 November (all `google_drive/`, gitignored):
`configs.pck`, `gfx.pck`, `grasses.pck`, `textures.pck`, `trees.pck`. There are
**no per-client `element/` folders**, so any item whose survey is per-client
(1.3.1/1.4.4/1.8.4 …) cannot run here and says so.

## Done

| # | item | what changed | how it was verified |
| --- | --- | --- | --- |
| 76 | `angelica_pck` kept a second copy of the retail revision table | `Revision`/`REVISIONS`/guard pair are `pub(crate)` in `pck`, `angelica_revision()` maps `0x00020001`/`0x00020002` onto the `0x00020002` row, and the Angelica reader takes length, guard offset and count from it | real `google_drive/configs.pck`: 58 entries, first payload decodes to the 286 bytes its record claims; a test asserts the two tables are the same, so a later edit to `REVISIONS` cannot leave `angelica_pck` behind |
| 77 | `flags_at & 0x80000000` refused as a bare string | `ENCRYPTED_FLAG`, `ENCRYPTED_REFUSAL` and `is_encrypted_refusal()`; the refusal names the revision and the `dwFlags` word. `pck_lib` raises `EncryptedPackage`; `archive-census.py` prints `SKIPPED:` and exits 0 | a copy of the real `configs.pck` with bit 31 set: both readers produce the same named refusal, the census reports a skip, and the unmodified pack still reads 58 entries |
| 82 | 28 tests silently `return`ed over an absent staged client | `pck::corpus_file_absent` counts and prints the skip and fails when `PWE_CORPUS=require`; all 28 sites converted. The offline harness's `pck` stub was a silent no-op too and now counts | the skip prints and increments; the `require` path panics; two tests pin both |
| 73 | `KEYPOINTCOUNT == "1"` stood in for a count | `common()` split from `keypoint()`, count read from the file (bounded at 8), every keypoint kept, `Definition` carries `keypoint_count`/`keypoint_index`. `InterpolateMode != 0` and an out-of-range count remain **named** refusals | see the correction below: **per element, all 488 light elements declare `KEYPOINTCOUNT: 1`**, so the parse/reject split over the 466 light-bearing graphs is 466/466 before *and* after. This is a correct-shape change, not a corpus win, and the code comments say so |
| 74 | DDS FourCC match `DXT1`/`DXT3`/`DXT5` only; no `DX10` | `tools/dds-survey.py` added; `Uncompressed` gained `luminance`; 8- and 16-bit `DDPF_LUMINANCE` parse and `model_texture_gpu` widens the channel into RGB | 1,433 DDS members across the four packs went from **59 refused to 0 refused**; the 59 were the whole `textures/water/caustic/` set, which drew as the white placeholder |
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
| 75 | `terrain_gpu.rs` requires `Format::Dxt1`, `mips.len()==1`, 256×256 for lightmaps | Needs per-client `element/` lightmap DDS to survey. Only `textures.pck` is staged, and its DDS set is 1,433 members none of which are terrain lightmaps. Relaxing to "any format, one mip" without evidence would accept multi-mip lightmaps the renderer cannot upload |
| 78 | `grass.rs` requires `nf == 1` and caps `nv>100_000`/`ni>300_000`; EC-BSD `n > 10000` | Measured `grasses.pck` (138 MOXB meshes): max `nv` 56, max `ni` 144, `nf` exactly 1 for all 138, so neither the frame-count equality nor the vertex/index caps are reachable from the staged data. The `n > 10000` name count needs an EC-BSD placement file and **no staged pack contains one** (`grasses.pck` and `trees.pck` were both scanned for the `0x42584f4d 0x42534446` identity: zero members) |
| 79 | caps at `scene.rs`, `pck.rs`, `water.rs`, `tree_source.rs`, `grass.rs` | The ones the staged data can measure are already documented beside their constant with the measured maximum (`scene.rs` ECWLD: 22,528 blocks, 59 tree types, 244 grass types; `water.rs`: 50 columns, 32 rows, 32 m cell size). The rest need an ECWLD/EC-BSD corpus: **no `.ecwld` is staged** |
| 80 | legacy 59-type SpeedTree bridge in `windows_setup.rs` | Needs `speedtreert.dll` plus a client whose tree type count is not 59, to show the variable-count path works. The DLL is not staged |
| 81 | rodio features in `Cargo.toml` | Needs a Windows build to test the audio backend selection; the offline toolchain has no rodio |
| 83 | `designer/catalog.rs` family `0x3000` | Needs an `elements.data` that is not v27 to survey the family word across clients. No `elements.data` is staged |
| 85 | split the 61-file PR into four | Explicitly ordered **after** the code items, so it is not started; splitting mid-change would fragment the commits it is meant to separate |

### The corpus this depends on

The packs are reachable through the Google Drive connector and, once downloaded,
land in the gitignored `google_drive/` folder. `configs.pck` (218 KB),
`gfx.pck` (13.7 MB), `grasses.pck` (9.0 MB), `textures.pck` (82.7 MB) and
`trees.pck` (19.1 MB) are staged this way. The per-client folders are not in the
Drive listing this checkout was given, which is what blocks 75, 78, 79, 80, 83.
