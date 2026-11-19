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

## `gfx.pck` measured in this checkout (18 November)

The table above predates `gfx.pck` being staged. It is now, and the same tool
runs on it:

| pack | members | supported | out of scope | unsupported |
|---|---:|---:|---:|---:|
| `gfx.pck` | 3499 | 3216 | 106 | **177** |

The 177 are exhaustive and none is a surprise: **174** are the fireworks
`.fw` files, a `MOXB` container of `FWGlyphText`/`FWFirework` children that no
`src/` reader touches (fireworks are not part of the eight phases); **1** is a
4-byte truncated `gfx/人物/技能/羽芒/翼击击中.gfx` whose whole content is the
ASCII stub `MOXT` -- the same 4-byte stub the `content` harness mode reports as
its single known failure; **1** is a zero-byte `gfx/场景/logs/af.log`; and **1**
is an OLE `gfx/烟花/效果.xls`, authoring data.

**What this run corrected.** The first reading of this pack said 2,616
unsupported, because the classification table listed neither `.gfx` nor `.sgc`.
Those two extensions are 2,439 members -- the entire reason the pack exists --
and `src/gfx_file.rs` parses both: they start `MOXTVersion:` exactly like the
effect graphs already in the table. The census was measuring its own table
rather than the code. `.fw` and a signature-based fallback for members whose
name lies (`gfx/怪物/通用/地狱狮王效果`, extensionless, holding a
`MOXTVersion: 41` graph) are now classified too, and
`tools/test_archive_census.py` pins the rules so the table cannot silently drift
again.

"Zero unsupported" here means: every member of these four packs is either parsed
by the viewer or positively identified as belonging to another subsystem, with
the reason recorded. It does **not** mean the viewer renders everything those
parsers accept at full fidelity. Seven packs are covered (`gfx.pck` as of 18 November);
`building.pck`, `litmodels.pck`, `surfaces.pck` and `loddata.pck` are not staged, and
BMD coverage therefore rests on the existing `litmodels.pck` census rather than
on this tool.

## DDS pixel formats measured (18 November)

`tools/dds-survey.py` is the survey for the one thing `model_dds.rs` dispatches
on: the `DDS_PIXELFORMAT`. It reports each format a pack carries, whether this
build reads it, and which accepted FourCCs no pack carries -- because a format
the reader accepts and nothing ships is dead code that reads like coverage.

Run over the four packs staged in this checkout, **1,433 DDS members**:

| format | members | read |
| --- | ---: | --- |
| `DXT3` | 651 | yes |
| `DXT1` | 613 | yes |
| 32-bit `00ff0000/0000ff00/000000ff` (+ alpha mask or not) | 77 | yes |
| 16-bit luminance (`DDPF_LUMINANCE`, flags `0x80000`) | 59 | yes, as of this change |
| 24-bit BGR | 33 | yes |
| `DXT5` | **0** | accepted by the reader, carried by nothing here |
| `DX10` | **0** | no DX10 header path is needed |

Two things came out of this.

**The 59 luminance members were a real loss, not a cosmetic one.** They are the
whole `textures/water/caustic/` set: 16-bit, masks `000000ff` / `0000ff00`, one
channel with the sample in the low byte of each word. The reader refused every
one, so each caustic light pattern drew as the flat white placeholder.
`Uncompressed` now carries a `luminance` flag and `model_texture_gpu` widens
the single channel into R, G and B. After the change all 1,433 parse.

**No `DX10` exists in any staged pack, so adding one would be speculative.**
Item 74 asked whether the FourCC match should grow a `DX10` case; the answer
from the data is no, and the reader still names an unknown FourCC instead of
dropping the member.

The survey also caught a mistake in its own first table: it reported the 33
24-bit members as unreadable, but `model_dds.rs` already accepts 24-bit BGR.
The table was wrong, not the reader -- so `READ_RGB` is now checked against the
parser rather than guessed from the pack. This is the same failure shape the
`gfx.pck` census had (measuring the table instead of the code), which is why
the two surveys live beside each other here.

## T2LM lightmap tile edges measured (18 November)

Item 75 asked whether `terrain_gpu.rs` should stop requiring DXT1 / one mip /
256×256 for a lightmap. `tools/lightmap-survey.py` answers it from files
rather than from the reader:

| file | header edge | tile bytes | DDS | mips | format |
| --- | --- | --- | --- | --- | --- |
| `login_login.t2lm` | **128** | 8,320 | 128×128 ×16 | 0 | DXT1 ×16 |
| `a91_6.t2lm` | **128** | 8,320 | 128×128 ×16 | 0 | DXT1 ×16 |
| `a85_6.t2lm` | **128** | 8,320 | 128×128 ×16 | 0 | DXT1 ×16 |
| `world_8.t2lm` | 256 | 32,896 | 256×256 ×16 | 0 | DXT1 ×16 |

**The 256 was wrong for three of the four.** `Tiles::parse` asserted
`u(12) == 256`, so it refused every login map and every 128-tile world map
outright — not a silent misread, a hard `unsupported T2LM version, tile
geometry or DDS flags` on files the client draws. The header at offset 12 is
the tile edge, and two values ship, so it is now read (`tile_side`) instead of
asserted, and anything other than 128 or 256 is refused *with the number it
saw*.

The DXT1 and one-mip parts were correct and stay enforced. The `world_8.t2lm`
record in `Findings/Phase-1-completion-checks.md` was therefore accurate about
the file it named and wrong as a general claim; the same 256 appears in
`Findings/Research-log-02-rendering-and-lighting.md` ("All 88 `.t2lm` and
`.t2lm1` files contain 16 DXT1 256² DDS images"), which the data contradicts.

One thing the survey settled without being asked: in all four files the header
edge and the DDS dimensions **agree**. `terrain_gpu` now requires that
agreement explicitly, so a file repacked at one size but retiled at another is
named rather than uploaded into a mismatched atlas slot.

### Why the reader could not have caught this itself

The offline suite compiles `terrain_lightmaps.rs` but never sees a client
`.t2lm`; the phase-1 check ran on one 256 file and generalised. That is the
recurring shape here — a claim measured once, written down as a rule, and then
enforced against data that was never re-measured.

## EC-BSD placement tables measured (18 November)

Item 78's open half was `grasses.pck`'s `ECBSD_NAME_LIMIT`: the constant was
documented as *unmeasurable*, because both `grasses.pck` and `trees.pck` were
scanned for the `0x42584F4D` / `0x42534446` identity and hold no such member.
That scan was correct and the conclusion was wrong — EC-BSD files are not pack
members. They live under `maps/world/`, one per map, and are only reachable in
a full client.

`tools/ecbsd-survey.py` over 8 real files:

| file | revision | names |
| --- | --- | --- |
| `a117.ecbsd` | 18 | 1 |
| `a79.ecbsd` | 17 | 1 |
| `a95.ecbsd` | 18 | 1 |
| `a64.ecbsd` | 18 | 4 |
| `login.ecbsd` | 18 | 4 |
| `a85.ecbsd` | 18 | 9 |
| `a91.ecbsd` | 18 | 12 |
| `world.ecbsd` | 18 | **88** |

**The measured maximum is 88**, so the limit sits 9,912 names above the largest
real table. Every file's name table parses completely under the three rules
`water::path_string` applies — length ≤ 1024, clean GBK, and no `..`, leading
`/` or `:` in the result — and both shipped revisions (17 and 18) read, which
`scene::ecbsd_header` already allowed because it gates on identity only.

The non-obvious part is what the names *are*: `BSData/1.dat` … `BSData/88.dat`
in `world.ecbsd`, and `BSData/Login.dat` first in `login.ecbsd`. The name table
indexes terrain grass-placement blocks, not textures, which is why 88 is a
plausible ceiling for a large world rather than an odd one.

### The pattern this is the third instance of

`grasses.pck` was scanned, nothing was found, and the absence was written down
as "unmeasurable" — accurate about the pack and false about the format. The
tile-edge mistake above is the same shape from the other direction: one file
measured, generalised to all. A survey answers the question it was pointed at,
so the register now records *where* each measurement was taken.

## The `.pck` containers measured across three clients (18 November)

A report of "unsupported PCK safe header" on 1.8.4 and a 1.5.2 client with no
buildings sent this back to the files. Three packs were read directly:

| client | pack | revision | trailer | guard pair | tag word | entries |
| --- | --- | --- | --- | --- | --- | ---: |
| 1.5.2 | `configs.pck` | `0x00020002` | 280 | `a508bdc4`/`853567c8` | `56a089b7` | 67 |
| 1.5.2 | `grasses.pck` | `0x00020002` | 280 | `a508bdc4`/`853567c8` | `56a089b7` | 103 |
| 1.8.4 | `configs.pck` | `0x00020003` | 288 | `fdfdfeee`/`f00dbeef` | **0** | 1,057 |

**The guard pair is the interesting one, because removing it was my own doing.**
Item 76 consolidated `angelica_pck`'s guard constants into `pck`'s on the stated
grounds that "`0xa508bdc4` / `0x853567c8` appears in no staged archive". It does
— it is what *every* 1.5.2 archive carries. The claim was a statement about what
had been looked at, made in the voice of a statement about the format, and it
refused the whole client. Both pairs are now named constants with the archive
each was measured on.

Two further offsets also move with the revision, and each was a separate
refusal:

| | `0x00020002` | `0x00020003` |
| --- | --- | --- |
| trailer bytes | 280 | 288 |
| record bytes | 276 | 288 |
| offset field | `+260`, 32-bit | `+264`, **64-bit** |
| `dwFlags` | `+12` | **`+16`** |

The `dwFlags` row is the one that would have been a false encryption report
rather than a refusal: reading `+12` on a `0x00020003` pack yields
`0xffffffff`, the first bytes of `szDescription`. `tools/pck_lib.py` had exactly
that bug, so every 1.8.4 package would have been censused as *enciphered*.

### What this says about the earlier registers

Two claims in `Findings/PART-3-survey-relax-2026-11-18.md` rested on the same
error and are corrected there: the EC-BSD "unmeasurable" note and this guard
pair. Both were produced by looking in one place and writing down a fact about
the format. That is now the third instance in this file, which is why each row
records where the measurement was taken.
