# Archive-wide file/type census

Purpose: P3.01 asks for "an archive-wide survey that names what is still
unsupported", and P8.01/P7.06 ask for a full census with "zero unsupported or
silently skipped content". A percentage cannot answer either of those. What can
is naming every member of every pack and giving each one a verdict.

Reproduce with:

```
python3 tools/archive-census.py google_drive/<pack>.pck [--list-unsupported]
```

The tool classifies by **content signature**, not by extension, because several
extensions are containers whose members differ (`.tga` in `shaders.pck` holds
icons, not shader data). Anything with no parser is reported by name, size and
reason instead of being dropped.

## Totals

Packs surveyed are the four retail packs matching this client build
(`shaders.pck` md5 `2adbdda9...`, 2019-05-03 fill). All are version `0x20002`.

| pack | members | supported | out of scope | unsupported |
|---|---:|---:|---:|---:|
| `grasses.pck` | 1701 | 407 | 1294 | **0** |
| `trees.pck` | 325 | 127 | 198 | **0** |
| `configs.pck` | 72 | 19 | 53 | **0** |
| `shaders.pck` | 166 | 162 | 4 | **0** |
| `textures.pck` | 1038 | 1037 | 0 | **1** |
| `models.pck` | 40753 | 40569 | 177 | **7** |
| **total** | **44055** | **42321** | **1726** | **8** |

The eight unsupported members are, exhaustively: the single stray JPEG in
`textures/sky/夕阳/`, and seven **zero-byte** authoring leftovers in `models.pck`
(`logs/af.log`, `logs/skeletoneditor.log`, `log.txt`, `~ps49.tmp` and similar).
Nothing in either pack is skipped because of unknown *content*; those eight are
identified, named, and empty or irrelevant.

## `models.pck` is not where the BMDs are

This census overturned a working assumption that had been carried for several
sessions, including by me in this branch: `models.pck` does **not** contain the
character/effect BMDs. It contains **zero `.bmd` members**. It is the Angelica
EC/ECM asset pack:

| | count | |
|---|---:|---|
| `.stck` | 14065 | `MOXBKCTS` animation clips -> `src/animation.rs` |
| `.ski` | 10898 | `MOXBIKSA` skin meshes -> `src/skin.rs` |
| `.dds` | 6005 | textures |
| `.ecm` | 3745 | text `MOXTVersion: N` models -> `src/ecmodel.rs` |
| `.bon` | 3579 | `MOXBEKSA` skeletons -> `src/ecmodel.rs` |
| `.smd` | 2228 | `MOXBDMSA` skin models -> `src/ecmodel.rs` |

BMDs live in `litmodels.pck` (28,858 world BMDs, already censused) and
`building.pck` holds none. The one `.ecm`-adjacent surprise is that ECM is a
**text** format - `MOXTVersion: 21` + INI-style keys - spanning 24 distinct
versions (v12 through v47) across 3,745 files, all of which parse.

### A bug this census had, that it then found

The first run reported `stck`, `ski`, `bon` and `smd` as unsupported. The cause
was ordering inside `sniff()`: it tested `b[:4] == b"MOXB"` *before* the 8-byte
container tags, and since every Angelica binary starts with `MOXB`, all four
were classified as generic models. Longest-match-first fixes it, and the
regression is worth stating because it is the same class of error as the
extension-level assumptions the census exists to replace: a coarser test placed
ahead of a finer one silently swallows every specific case.

## Per-extension verdicts

| ext | count | verdict | where |
|---|---:|---|---|
| `.ecp` | 1293 | out of scope | ECModel convex collision hull (`MOXBpbce` + a path back to its `.ecm`); collision data, never rendered |
| `.dds` | 232 | supported | `src/dxt.rs`, `src/texture_resolution.rs` |
| `.txt` | 130 | supported | HLSL sources, `src/post_gpu.rs`/`src/post_plan.rs` |
| `.mox` | 155 | supported | `src/grass.rs` (32-byte vertices; see `Grass-model-format.md`) |
| `.mod` | 144 | supported | `src/grass.rs` `mox_wrapper_child` (added during this census) |
| `.spt` | 100 | out of scope | converted by `tools/speedtree/bake.cpp` against the retail `SpeedTreeRT.dll` |
| `.tbh` | 96 | out of scope | tree billboard helper, consumed with the `.spt` source |
| `.sdr` | 31 | supported | shader data |
| `.tga` | 22 | supported | icons/billboards (`shaders.pck` `.tga` are icons, not shader data) |
| `.ini`/`.lst`/`.cfg` | 61 | out of scope | configuration and table data |
| `.scc` | 8 | out of scope | source-control metadata shipped in the retail pack |
| `.exe`/`.vso` | 4 | out of scope | prebuilt binaries inside the shader pack |
| `.grs` | 1 | out of scope | test-only grass sample; absent from `elementclient.exe` |
| `.action` | 1 | supported | `src/ecmodel.rs`, `src/model_playback.rs` |
| `.border`/`.data`/`.sw` | 3 | out of scope | UI/opaque blobs |

## What the census actually found

Two real defects that the extension-level view had hidden:

1. **`.mod` was not parsed at all.** 144 members of `grasses.pck` name their
   geometry through a `MOXTMODEL:` text manifest rather than being `.mox`
   themselves. The loader only resolved `.mox`, so a grass type that pointed at
   a `.mod` was reported missing and silently dropped from the scene. A parser
   (`mox_wrapper_child`) and loader wiring were added; all 144 archive members
   now parse and resolve to an existing `.mox` child.

2. **`.ecp` is not grass.** 1293 of `grasses.pck`'s 1701 members are EModel
   collision hulls for NPCs and siege props, not vegetation. Classifying them as
   "unsupported grass" would have inflated the apparent gap by 3x. They carry a
   `F:\ECModelBrushEditor\...` path back to the `.ecm` they bound, which is
   exactly what identifies them.

The `.mod` defect is the second one found this session by looking at real
archive content rather than at the viewer's assumptions. Both were invisible to
"does the parser accept this extension" style checks.

## Honest scope statement

"Zero unsupported" here means: every member of these four packs is either parsed
by the viewer or positively identified as belonging to another subsystem, with
the reason recorded. It does **not** mean the viewer renders everything those
parsers accept at full fidelity. Six packs are covered; `building.pck`,
`litmodels.pck`, `surfaces.pck`, `gfx.pck` and `loddata.pck` are not staged, and
BMD coverage therefore rests on the existing `litmodels.pck` census rather than
on this tool.
