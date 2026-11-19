# Cross-version reading matrix

What this viewer can actually read, per retail client version, measured rather
than assumed. Every number below comes from running the shipped readers over
files taken from those clients; nothing here is inferred from a version number.

The clients cannot be launched in the environment these measurements were taken
in (no Wine, no GPU path), so "reads X" always means *the Rust reader consumed
the file and produced these counts*, not *the game rendered it*.

## How to reproduce

```
tools/offline_harness.sh matrix    # one row per staged client
tools/offline_harness.sh pck       # every entry of every pack, decoded
tools/offline_harness.sh content   # the files inside them, by owning reader
tools/offline_harness.sh scene     # every .ecwld
tools/offline_harness.sh gfx       # GFX containers (file, decal, light) + tests
```

Each mode takes a directory argument, defaults to `google_drive/clients` (or
`google_drive/packs`), and exits non-zero if anything fails to read. They are
the same readers the application uses — no probe code is duplicated.

## Client coverage

| | 1.3.1 | 1.3.6 | 1.4.2 | 1.4.4 | 1.5.2 |
|---|---|---|---|---|---|
| `elementclient.exe` | — | — | **1.4.2**, fingerprint match | — | unnamed build, PE32 |
| `elements.data` | v10 | v12 | v27 | v70 | v127 |
| `tasks.data` | v55 | v56 | v75 | v105 | v121 |
| `gfx.pck` | 3765 entries | 3499 entries | — | — | — |
| `configs.pck` | — | 66 entries | — | 92 entries | 89 entries |
| `shaders.pck` | — | — | — | — | 371 entries |
| `script.pck` | — | — | — | — | 2 entries |
| `.ecwld` scenery | — | — | — | a37 ✓ | a07 ✓ |
| water areas | — | — | — | a37: 3 | a07: 1 |

A dash means that file was not staged in the measured corpus, **not** that the
reader fails on it. `elements.data`/`tasks.data` rows are the version words the
files declare, which is all the Librarian and Character Designer need to report
honestly; those sections read the v27 layout and stop on anything else.

### Package revisions

All three container revisions the client dispatches on are read:

| revision | trailer | entry record | seen in |
|---|---|---|---|
| `0x00020001` | 272 B | 276 B | older packs |
| `0x00020002` | 272 B | 276 B | 1.3.6, 1.4.4, 1.5.2 packs |
| `0x00020003` | 280 B | 288 B, 64-bit `dwOffset` | 1.5.2 `models.pck` |

`0x00020003` was refused outright before this work (`unsupported PCK version
00020003`). Verified end to end: `gfx.pck` from 1.3.6 parses to 3499 entries and
**all 3499** decode to the length their record claims.

### Two containers, told apart by the guard pair

A `.pck` is not always this package. `configs.pck` and `shaders.pck` ship in a
second Angelica container that shares the 12-byte prefix
(`0x4dca23ef`, size, `0x56a089b7`) and the 280-byte trailer, but differs in two
ways that matter to a reader:

| | retail v2 (guard `0xfdfdfeee`/`0xf00dbeef`) | Angelica (guard `0xa508bdc4`/`0x853567c8`) |
|---|---|---|
| directory | a forward table at `dwEntryOffset ^ 0xa8937462` | a run of zlib records walked *backwards* from the trailer |
| record | length-prefixed, masked, optional zlib | 8-byte prefix then a zlib stream of one 276-byte record |

The guard pair is therefore not a spare key: reading `dwEntryOffset ^ MASK` on
the `0xa508bdc4` pair gives an offset **past EOF** on both packs staged here
(`script.pck` `0x8950c58f` vs 44 330 bytes; `shaders.pck` `0x892d59dd` vs
8 225 973 bytes), while on the `0xfdfdfeee` pair it lands inside the file. Each
reader keeps to its own container; teaching `src/pck.rs` to accept both guard
pairs would have it seek past the end of the config packs.

Measured, in every case all entries decoding to their declared length:

| pack | container | entries |
|---|---|---|
| `gfx.pck` (1.3.1) | retail v2 | 3765 |
| `gfx.pck` (1.3.6) | retail v2 | 3499 |
| `configs.pck` (1.4.4) | retail v2 | 92 |
| `configs.pck` (older, 1.69 MB) | retail v2 | 106 |
| `shaders.pck` (8.23 MB) | Angelica | 371 |
| `script.pck` (1.5.2) | Angelica | 2 |

The 1.5.2 `script.pck` is the only staged archive carrying a version marker at
all: its two entries are a zero-length `dir` and a bare `version.sw` holding
`0.525.0`. Both are now reported, so a client card reads
`script.pck  Angelica pack v0x20002 · 2 entries · version.sw 0.525.0`. The
lookup matches the bare name as well as the `<stem>\version.sw` a patcher
writes, because this one uses the bare spelling.

## Package inventory

Fifteen staged packages, every entry decoded and checked against the length its
record declares — 12 806 entries, 0 failures:

| pack | container | entries | bytes out |
|---|---|---|---|
| `gfx.pck` (1.3.1) | retail v2 | 3765 | 51 972 120 |
| `gfx.pck` (1.3.6) | retail v2 | 3499 | 46 496 028 |
| `surfaces.pck` | retail v2 | 2285 | 127 318 710 |
| `interfaces.pck` | retail v2 | 1158 | 7 536 348 |
| `trees.pck` | retail v2 | 325 | 48 643 974 |
| `shaders.pck` | Angelica | 371 | 15 865 920 |
| `configs.pck` (older) | retail v2 | 106 | 11 347 918 |
| `grasses.pck` | Angelica | 103 | 4 014 681 |
| `configs.pck` (1.4.4) | retail v2 | 92 | 6 909 162 |
| `configs.pck` (1.3.6) | retail v2 | 66 | 3 160 870 |
| `configs.pck` (1.5.2) | retail v2 | 58 | 2 179 038 |
| `script.pck` (small) | retail v2 | 32 | 51 717 |
| `textures.pck` | retail v2 | 942 | — |
| `fonts.pck` | retail v2 | 2 | 8 242 268 |
| `script.pck` (1.5.2) | Angelica | 2 | 9 |

Note `grasses.pck` is in the Angelica container while `trees.pck` beside it is a
retail v2 pack — the container is a property of the pack, not of the client
version or the content type.

## Reading the files inside

A directory that parses proves nothing about its members, so the `content` mode
runs the owning reader over each one. Over the same corpus: **9 190 members
read, 3 failed**.

| extension | count | check |
|---|---|---|
| `gfx` | 3944 | `gfx_file::parse` after a GBK decode |
| `spt` | 100 | `__IdvSpt_0` magic at offset 8 (no parser: SpeedTree source) |
| `bon`, `ski` | 148 | length; containers with no leaf parser here |
| `sdr` | 60 | shader text |
| `mox` | 44 | `MOXBFRAME:` magic — **no reader in this project** |
| `dds`,`tga`,`bmp`,`jpg` | 4891 | magic and header, not pixels |
| `ttf` | 2 | sfnt magic (`00 01 00 00`), not glyphs |

Images are checked by header rather than by pixel: the offline toolchain has
only a stub `image`, but DDS carries a magic and a 124-byte header, TGA a type
field, BMP and JPEG their own magics, which is enough to catch truncation.

| extension | count | result |
|---|---|---|
| `dds` | 3604 | 3603 valid; **1 is machine code** (below) |
| `tga` | 856 | all valid |
| `bmp` | 404 | all valid |
| `jpg` | 27 | all valid |

Three members across the corpus are named exceptions rather than reader faults,
and are excluded from the exit status:

- `gfx/人物/技能/羽芒/翼击击中.gfx` in **both** `gfx.pck` builds — four bytes,
  `MOXT`.
- `interfaces/iconlist_guild.dds` — 5 MB of x86 machine code (`8b 44 24 04`,
  `c2 04 00` thunks) with long runs of `$` and three embedded `DDS ` strings
  whose header sizes are nonsense. Every 64 KiB window stays under 7.5 bits/byte
  and 42.6% is printable-or-zero. The pack's 1158 spans have **zero overlaps**
  and this member sits exactly where the byte stream places it, so it is the
  archive's content, not a misread offset.

What this still does *not* cover: none of these payloads are decoded to pixels
offline. `.xls`, `.lua`, `.txt`, `.ini`, `.dtf`/`.stf`/`.htf` (interface
descriptors), `.sgc`/`.scc` (script) and the remaining extensions are opaque
blobs to this project and are counted in the inventory only.

Robustness over the four largest packs (mutation of the trailer and record run,
never the payload): 1 000 rounds, 130 accepted, 870 refused, **0 panics**.

## Scenery

Six `.ecwld` files parse, from small instance maps to full world maps:

| file | blocks | grid | block | tree types | trees | water | grass | ornaments | effects | critters | sounds |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `world` (2.78 MB) | 22528 | 176×128 | 64 | 59 | 3646 | 9039 | 38796 | 41494 | 3535 | 1347 | 1044 |
| `world` (2.45 MB) | 22528 | 176×128 | 64 | 59 | 3938 | 9204 | 34896 | 26647 | 2275 | 1347 | 827 |
| `a37` (Morai, 1.4.4) | 4608 | 48×96 | 64 | 0 | 0 | 148 | 2704 | 8441 | 362 | 0 | 0 |
| `a70` | 2304 | 48×48 | 64 | 0 | 0 | 0 | 0 | 2853 | 1908 | 0 | 0 |
| `b01` | 256 | 16×16 | 64 | 0 | 0 | 0 | 0 | 605 | 205 | 0 | 0 |
| `a07` (1.5.2) | 256 | 16×16 | 64 | 0 | 0 | 26 | 0 | 202 | 22 | 0 | 0 |

`trees = 0` on the maps where `tree types = 0` is not a parser fault: those
clients place vegetation as **ornaments**, and a map with no tree-type table
cannot carry tree instances. The two world maps, which do carry 59 tree types,
report thousands of trees — the tree path is exercised.

## Water

Water is a two-level structure: `maps/<id>/<id>.ecbsd` is a manifest of
`bsdata/*.dat` paths plus a per-water-ref record index, and the areas live
inside those `.dat` files.

| map | water refs | unique refs | areas |
|---|---|---|---|
| `a07` (1.5.2) | 26 | 1 | 1 |
| `a37` (1.4.4) | 148 | 3 | 3 |

Refs collapse to unique `(export_id, offset)` pairs before loading, which is why
148 refs become 3 areas rather than 148. Both numbers are correct and are
reported separately so neither hides the other.

An empty `bsdata` record is 16 bytes — `MOXB`, version 2, one entry, offset 0 —
which is an authored-empty record rather than a truncated one. Several of the
18 `a37` records are exactly that.

### Every water reference, bucketed

"3 areas out of 148 references" looks like a fault until the references are
counted by what happened to them. `water-record-survey --classify` puts every
reference in exactly one bucket and fails if the buckets do not add up, so the
question "where did the other 145 go" is answered rather than asked.

```
tools/offline_harness.sh water-record-survey --classify <element/maps>
```

| map | refs | unique | areas | empty record | unknown version | unknown format | read OK |
|---|---|---|---|---|---|---|---|
| `a07` (1.5.2) | 26 | 1 | 1 | 0 | 0 | 0 | 26 |
| `a37` (1.4.4) | 148 | 3 | 3 | 0 | 0 | 0 | 148 |
| `world` (1.8.4) | 9237 | 111 | 109 | 0 | 0 | 256 | 8981 |

The refs collapse because a map names every water area once per block that can
see it: `a37`'s 148 references are 3 unique `(export id, offset)` pairs and
`world`'s 9237 are 111. Every reference decodes; nothing is lost.

### 1.8.4, the first time it was measured

The 1.8.4 world is the first client whose water was measured rather than
assumed. Its 88 `bsdata/*.dat` patches and its `world.ecbsd` (2.2 MB, 111
records) were staged and run through the classifier:

- **0 unknown versions.** All 112 water records outside the framed patches open
  with `0x10003000`, the layout this reader decodes.
- **0 empty records.** Unlike 1.5.2's maps, no 1.8.4 patch is the authored-empty
  16-byte stub.
- **256 references (2 patches) are not water records at all.** `bsdata/32.dat`
  record 0 and `bsdata/45.dat` record 59 inflate cleanly to 32,768 bytes and
  contain no `0x1000xxxx` version word anywhere, so the record carries other
  BSData data and names a water record index that is not water.
- **109 areas decoded.** The gap between 111 unique refs and 109 areas is the
  two patches above.

The first pass reported those 256 references as *unknown version*, and the error
that produced it was `unsupported A3DWaterArea version 0x1af`. `0x1af` is 431 —
the record's **zlib stored length**, not a version. The reader had been
reporting a framing word as a version since the original 1.3.1 investigation.
That is fixed: a record that inflates but holds no water now says so, and only a
real `0x1000xxxx` revision word this build has no layout for is reported as a
version. The bucket names follow the same rule, so a future survey cannot
resurrect the mistake.

## Known gaps, stated plainly

- **1.5.2 `gfx.pck` (256 MB) is not in the corpus.** Upload/download caps make it
  unreachable, so the GFX survey covers 1.3.1 and 1.3.6 only. Those two carry
  GFX versions 28–58 between them and all of them parse.
- **Encrypted packages are refused, not read.** The client supports them behind
  a flag; no retail pack in the corpus sets it, so the refusal is honest rather
  than guessed.
- **Split packages are named, not followed.** A `.pck` whose prefix declares
  more bytes than the file holds is reported as `split package … the rest is in
  <stem>.pkx`. Reading across the split is not implemented.
- **`elementclient.exe` version resources** identify 1.4.2 exactly. The 1.5.2
  binary has no `RT_VERSION` resource at all, so it is reported as an unverified
  build with its hash rather than assigned a version.
- **Interface limits are not versions.** Elements/tasks data above v27 are
  reported with their version and the sections that need the v27 layout stop
  there; that is a viewer limitation, and the matrix says so rather than
  pretending the file is unreadable.

## Map matrix (`matrix-maps`, 18 November 2026)

One line per map per client, produced by `tools/offline_harness.sh matrix-maps
<clients dir>` with the same readers the viewer uses. A map that fails prints its
error and the mode exits non-zero.

| client | map | blocks | grid | tree types | trees | water refs | grass | ornaments | effects | models | sounds | precinct |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1.8.4 | `a37` | 4608 | 48x96 | 0 | 0 | 148 | 2704 | 8441 | 362 | 0 | 0 | 383 zones |
| 1.8.4 | `world` | 22528 | 176x128 | 59 | 3646 | 9237 | 38240 | 39179 | 2971 | 76 | 1044 | 383 zones |

`a37`'s 148 water references are the number the water classifier is required to
account for, and they are all read: 3 areas, 18 shoreline waves, 3,146 points.

### Instance maps (`instance`, 18 November 2026)

`tools/offline_harness.sh instance <maps dir>` loads every instance folder the way
the viewer does and prints one row per map. For the 1.8.4 `a37`, whose files are
staged:

```
map    ecwld    blocks    extent   zones centres tracks  english names
a37    v          4608 6144x3072     383     383    783  Upcoming / The Harshlands
```

Every one of the 383 zones has a centre, all 783 playlist tracks survive, and
the zone names reach English. `extent` is the map's own `.trn2` header, which
carries no world transform; see `Findings/Unsupported-client-paths.md` for the
one part of that question still open (the shared coordinate frame).
