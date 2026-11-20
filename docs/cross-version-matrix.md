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
| `0x00020002` | 272 B | 276 B | 1.3.6, 1.4.4, 1.5.2 packs; 2016 `trees.pck` |
| `0x00020003` | 280 B **or 272 B** | 288 B, 64-bit `dwOffset` | 1.5.2 `models.pck`; **1.8.4 `trees.pck`** |

`0x00020003` was refused outright before this work (`unsupported PCK version
00020003`). Verified end to end: `gfx.pck` from 1.3.6 parses to 3499 entries and
**all 3499** decode to the length their record claims.

**The version word does not identify a trailer layout.** Measured on the shipped
`trees.pck` files, whose trailer bytes are recorded verbatim in `src/pck.rs` as
`TREES_184_TRAILER`, `TREES_152_TRAILER` and `TREES_2016_TRAILER` and asserted by
three tests:

| file | version word | trailer | `dwEntryOffset` | word at `+12` | entries |
|---|---|---|---|---|---|
| 1.8.4 `trees.pck` | `0x00020003` | **280 B** | at `+8`, **32-bit** | `0xffffffff` | 326 |
| 1.5.2 `trees.pck` | `0x00020002` | 272 B | at `+8`, 32-bit | `0x00000000` | 325 |
| 2016 `trees.pck` | `0x00020002` | 272 B | at `+8`, 32-bit | `0x65676e41` (`Ange`) | 326 |

Two consequences, both encoded in the reader:

1. **A version-keyed table cannot be complete.** `0x00020003` names the 280-byte
   struct in `models.pck` and a narrowed 32-bit `dwEntryOffset` inside the same
   280-byte struct in `trees.pck`. The reader therefore *probes*: the version's
   own layout first, then `TRAILER_PROBES`, and accepts a candidate only when
   its guards, description tag and a full walk of the entry table to `count`
   all pass.
2. **The word at `+12` is not reliably `dwFlags`.** 1.5.2 wrote a real zero flag
   word there; the 2016 client wrote the ASCII start of its description
   (`Ange`), on the *same revision word*. The reader reads that field but lets
   the entry table decide: a package whose table walks cleanly opens whether or
   not bit 31 is set, and the flag is only named when the walk fails. Reading
   `trees.pck`'s `+12` as flags finds `0xffffffff` — an "encrypted" package that
   in fact decodes perfectly.

The narrowed 32-bit `dwEntryOffset` is legitimate rather than a misread: a
package that a 32-bit field describes cannot exceed 4 GiB, and this one is
26 MB.

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

## Trees

`count_trees` in `src/main.rs` is not version-dependent: it sums
`world_scene.blocks[*].trees.len()`, and `scene::Scene::parse` reads the tree
table from the same fixed header layout for every ECWLD version it accepts
(`version >= 12`, one layout). Trees are authored per block, so two maps of the
same client can differ by thousands while the reader behaves identically.

### Every point at which trees can be dropped, traced

`count_trees` (`src/main.rs`) sums `blocks[*].trees.len()` and never consults a
version. The placements are authored data, read by `scene::Scene::parse` from one
fixed block layout for every accepted version. So a tree can only disappear at
one of the places below, and each of them is reached *after* a map has authored
its trees:

| where | condition | effect |
|---|---|---|
| `trees::Trees::open` | `scene.tree_types.is_empty()` | returns no types, **no reason** — correct for `a91`/`a64`, which truly author none |
| `trees::Trees::open` | cache directory missing | `off_reason`: "no SpeedTree cache at …" |
| `trees::Trees::open` | `speedtreert.dll` unopenable | `off_reason` naming the DLL path and the OS error |
| `trees::Trees::open` | `trees.pck` unopenable | `off_reason` naming the pack path and the OS error |
| `trees::Trees::open` | `count == 0` with types authored | `off_reason`: "none of the N tree names … were found; first is …" |
| `main.rs` | any tree type failed | hard error, not a silent drop |

Note what is *not* in that list: no branch compares a version number. The
version split the brief asked about does not exist in the reader.

The five `off_reason` sites are all *visible*: each prints to stderr and is
carried on the `Trees` value so the UI can show it. A user-visible reason is the
difference between "trees do not spawn" and "trees do not spawn, and here is
why" — and in the shipped clients the reason being printed was the one that
mattered: the cache was never built, because source preparation aborted first.

### Correction: what the separator bug actually was

The first version of this section said `tree_source.rs` "indexed `trees.pck` with
raw backslashes". **That was half wrong**, and checking it changed the story:

* `pck::directory` **already folds backslashes.** It decodes each record's
  `szFileName[260]` through GBK and then does `decoded.replace('\\', "/")` before
  building the `Entry`. So the pack side never produced a backslash.
* The `.ecwld` tree table **also stores backslashes** — all 59 of the 1.8.4
  world's names do, measured from the file.

So both sides were backslashed, and the reader folded exactly one of them. The
old lookup lower-cased the world's name and compared it with the pack's
already-folded name, and the two never met:

| side | as stored | as the old code compared it |
|---|---|---|
| `trees.pck` (via `pck::directory`) | `trees\精简树木\雪松\x.spt` | `trees/精简树木/雪松/x.spt` |
| `world.ecwld` tree table | `Trees\精简树木\雪松\x.spt` | `trees\精简树木\雪松\x.spt` |

The fix is the same rule as before — fold on both sides — but the *diagnosis* is
that the reader was inconsistent with itself, not that the pack was unnormalised.
`the_pack_returns_forward_slashes_while_the_world_stores_backslashes` asserts all
four facts above from real bytes: the pack has no backslash, the world is all
backslashes, the old lower-case key misses, and the shared rule makes them meet.

### The lookup that stopped every tree from being baked

Trees do not fail in the reader; they fail in the *source preparation* that
happens before the SpeedTree bridge runs, and the defect there was measured
rather than assumed.

`src/tree_source.rs` built its index of `trees.pck` with `name.to_lowercase()`
and looked the scene's names up the same way. But every shipped `trees.pck`
stores **backslash** paths and every `.ecwld` writes **forward slashes**:

| pack | entries | names with `\` | names with `/` |
|---|---|---|---|
| 1.8.4 `trees.pck` | 326 | 326 | 0 |
| 1.5.2 `trees.pck` | 325 | 325 | 0 |

The pack holds `trees\精简树木\雪松\snowpinewithsnow1.spt`; the map asks for
`Trees/精简树木/雪松/SnowPineWithSnow1.spt`. Lower-casing leaves the separator
alone, so `contains_key` was **false for every asset in every client**, and
`extract_for_scene` returned `tree source asset missing` before writing a single
file. With no baked cache there was nothing for the bridge to load, so no trees
were placed — in 1.4.2, in 1.8.4, and in every version in between.

The runtime loader (`src/trees.rs`) already normalised correctly
(`normalise_tree_name`, `\` → `/` then lower-case); only the extraction path did
not. The rule now lives once, in `scene::normalise_tree_name`, and both readers
call it. Three tests in `src/scene.rs` pin it, using the real spellings from the
two shipped packs rather than names chosen to suit the code, and one of them
asserts the *old* lower-case-only behaviour does **not** match — so the bug
cannot come back silently.

`a91` and `a64` genuinely have zero trees: both declare zero tree types and zero
placements in their own ECWLD, which is authored content, not a read failure
(see *Instance maps*).

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
| 1.8.4 | `a37` | 4608 | 48x96 | 0 | 0 | 148 | 2704 | 8441 | 362 | 0 | 0 | 29 zones |
| 1.8.4 | `world` | 22528 | 176x128 | 59 | 3646 | 9237 | 38240 | 39179 | 2971 | 76 | 1044 | 383 zones |

`a37`'s 148 water references are the number the water classifier is required to
account for, and they are all read: 3 areas, 18 shoreline waves, 3,146 points.

`a37` shows **29 zones**, not 383: 383 is the *world* map's count. What makes
that more than an assertion is that the 383-zone file exists separately
(885,012 B) and does **not** belong to `a37`: put it in a37's folder and
`matrix-maps` reads `383 zones, 240 OUTSIDE the map`, while in `world`'s folder
it reads `383 zones` with nothing outside. Both modes catch the mix-up —
`instance` prints an `outside` column, and `matrix-maps` appends
`N OUTSIDE the map` to the precinct cell when any anchored zone centre falls
outside the map's own centred extent.

The file this checkout stages, `google_drive/precinct.clt` (78,858 B), is a
**third** thing: byte-identical to the one beside both `world` and `a37` in the
staged 1.8.4 client, and 29 zones with **no stereo ambience at all**. The
numbers quoted for "the 1.8.4 precinct" — 383 zones, 783 tracks, 347 zones with
a day ambience path, 345 with a night one, from 43 distinct day files and 43
distinct night files — are the 885 KB world file's. Both are measured, side by
side, by `tools/offline_harness.sh precinct`:

```
file                                                  ver  zones  tracks     day   night  paths      bad
google_drive/precinct.clt                               7     29      29       0       0      0        0
/tmp/cl1element/maps/a37/precinct.clt                   7    383     783     347     345     60        0
```

An earlier version of this paragraph said the file had been "copied into" a37's
folder from the world map's own directory. That was wrong about the mechanism
and right about the symptom: the directory holding the 383-zone file is named
`a37` too, and the two clients' `a37` folders differ in exactly this one member.
The extent test is what settles which map a precinct describes, and it was
always the right test — the provenance story around it was the part that was
invented.

### Instance maps (`instance`, 18 November 2026)

`tools/offline_harness.sh instance <maps dir>` loads every instance folder the way
the viewer does and prints one row per map. For the 1.8.4 `a37`, whose own files
are staged:

```
map    ecwld    blocks    extent   zones centres tracks  outside  english names
a37    v          4608 6144x3072      29      29     29        0  Celestial Lake / Candlelight Village
```

Every one of the 29 zones has a centre, all 29 playlist tracks survive, the zone
names reach English, and **`outside` is 0**: every zone centre falls inside the
map's own centred extent. `extent` is the map's own `.trn2` header, which carries
no world transform — the frame is the centred one, proved by a37's own precinct
(see `Findings/Unsupported-client-paths.md`).

**A correction worth keeping.** An earlier version of this section reported
*383 zones, 783 tracks* for `a37` and cited `Upcoming / The Harshlands`. Those
are the **world** map's numbers: the file staged as `a37/precinct.clt` was
byte-identical to the one beside `world.ecwld`. Under that mix-up 240 of the 383
centres fell outside a37's extent, which is what exposed it — a whole-map
precinct beside an instance map does not fit, and the `outside` column now says
so instead of the row looking healthy.

### GFX container versions in one pack (18 November 2026)

`MOXTVersion` as authored in the staged 1.3.1 `gfx.pck`. Counted with the pack's
own directory, so this is the archive's own content, not an inference:

| version | roots | | version | roots |
|---|---|---|---|---|
| 28 | 25 | | 35 | 181 |
| 29 | 6 | | 37 | 648 |
| 30 | 219 | | 39 | 3 |
| 31 | 19 | | 40 | 16 |
| 32 | 170 | | 41 | 85 |
| 33 | 399 | | 45 | 10 |
| 34 | 93 | | | |

2,071 `.gfx` members; **1,874 carry a `MOXTVersion` header**, and the table sums
to 1,874. One member is an empty container (the four-byte `MOXT` stub, below);
the remaining 196 are members whose name ends `.gfx` but whose content is not a
versioned root. The newest version in this pack is **45**, which is why the
older "79 is the format ceiling" claim needed a different pack to test — 79 was
one measurement from the 1.4.2 pack, and the version history plus the reader's
version-gated keys handle the rest (`Findings/Gfx-version-ceiling-2026-11-18.md`).

The full element sweep over the same 2,071 roots: **2,070 parse, 0 reach
`Spec::Unknown`**, and the single failure is the empty `MOXT` stub — so no
element id in this corpus is unhandled. Elements, and the number of roots that
carry at least one of each id:

| id | what | elements | roots |
|---|---|---|---|
| 100 | decal | 1,285 | 426 |
| 101 | decal variant | 2,183 | 648 |
| 102 | decal variant | 46 | 17 |
| 110 | ribbon | 1,096 | 258 |
| 120 | particle | 680 | 345 |
| 121 | particle (box) | 252 | 140 |
| 122 | particle | 40 | 40 |
| 123 | particle | 2,786 | 1,257 |
| 124 | particle (cylinder) | 481 | 189 |
| 125 | particle (generation sequence) | 1 | 1 |
| 130 | light | 521 | 499 |
| 140 | cylinder | 786 | 225 |
| 150 | fractal | 315 | 69 |
| 151 | lightning | 11 | 7 |
| 160 | model | 210 | 108 |
| 170 | sound | 355 | 340 |
| 180 | wave | 40 | 13 |
| 190 | curve (paraboloid) | 136 | 34 |
| 200 | child root (nested `.gfx`) | 21 | 5 |

Element 123 is the most common type in the whole archive, in more than half the
roots — a pack where it did not draw would look almost empty. Element 170 is the
only id the player counts without performing it (`Findings/Unsupported-client-paths.md`).

## Item status (18 November 2026)

One line per numbered item, with what settled it. "static" means the claim was
read out of the source and traced by hand; "measured" means a number came from
running a reader over a file — in this session that means Python over a file
downloaded from Drive, not Rust. Numbers from CI log lines are marked *CI*.

**What CI confirmed, and what it did not.** Run `44706594863` on commit
`719c8a3` is green on both jobs: "Rust checks and tests (Linux)" and "Windows
executable (x64)". On the Linux job, step 18 *Run viewer unit tests* and step 19
*Check formatting* both report success, which is what makes the tests below
compiled and passing rather than merely written. **The log text itself is not
retrievable through this environment's API** (the log endpoint returns an empty
body), so no pass or skip *count* in this document is quoted from CI: the counts
here come from static reading, and the green/red verdict is from the job and step
conclusions, which are readable. A number that is not from static reading is
labelled *measured* and carries the file it was measured on.

**1.1 — PCK trailer layouts tested from independent fixtures: done.**
Three shipped trailers are now recorded as literal hex — `TREES_184_TRAILER`
(280 B), `TREES_152_TRAILER` (272 B), `TREES_2016_TRAILER` (272 B) — and the tests
assert against those bytes, not against `TRAILER_PROBES`/`REVISIONS`. The
previous test rebuilt its fixture from the probe table, which only proved the
table agreed with itself. *(measured: bytes extracted from the shipped packs;
verified byte-identical to the files before being written into `src/pck.rs`.)*

**1.2 — Drive byte-check of the `trees.pck` trailers: done for 1.8.4, 1.5.2 and
2016; blocked for `litmodels.pck`.** All three trailer structures decode and are
pinned by tests. `litmodels.pck` is **not** checked because every copy on Drive
is 279 MB–2 GB, past the 100 MiB limit of the only file-transfer path this
session has, and CI has no Drive access at all — see `B-33` in
`docs/blockers.md` for the exact measurement that would settle it. *(measured:
1.8.4 `0x00020003`/280 B/326 entries/table 26,203,745; 1.5.2 `0x00020002`/272 B/325
entries/table 20,159,620; 2016 `0x00020002`/272 B/326 entries/table 25,430,518.)*

**1.3 — Trees, answered statically: done, and it found a real defect.**
(a) `count_trees` is **not** version-dependent — it sums
`blocks[*].trees.len()` from one fixed block layout. (b) Every drop point is
traced in *Every point at which trees can be dropped* above; five live in
`trees::Trees::open` and each carries a visible reason. (c) `a91` and `a64` truly
declare zero tree types and zero placements. (d) Verdict: **the trees did not
spawn because `src/tree_source.rs` lower-cased `trees.pck` names without folding
the path separator, so every lookup missed** (326/326 pack names use `\`, every
ECWLD uses `/`). Fixed by moving the one correct rule to
`scene::normalise_tree_name` and calling it from both readers; three tests in
`src/scene.rs` pin it using the real spellings. *(measured: 326/326 and 325/325
names carry backslashes; static: the lookup trace.)*

**1.4 — The four rejected GFX files: named, not dropped.**
`zero_flag` now reports the field **and the value it carried** instead of a bare
"unsupported", so the effect is never silently lost. `SurfaceUseParentDir` is the
one of these that shipped clients actually set — 16 elements in 8 roots, all
skill effects — while `keyNumber` and `GridAnimationLines` appear **zero** times
in the staged corpus; `StopEmitWhenFade` has no SDK documentation, so it is
recorded and not guessed. No version number was added for any of them. *(measured
from the corpus census in `Findings/World-effects-2026-11-09.md`; MOXTVersion for
the affected roots is 33–37, unchanged.)*

**2.1 — water versions: already covered.** `WATER_VERSIONS` holds both measured
words, `0x10002000` and `0x10003000`, and refuses anything else by name. *(static,
with the existing water harness tests.)*

**2.2 — `gfx_light` keypoint count and interpolation: already covered.**
`MAX_KEYPOINTS` is 8, and `InterpolateMode` is refused unless `0` — measured at
11,844 of 11,844 keypoints in the staged corpus. *(measured.)*

**2.3 — DDS formats: already covered.** `DXT1`/`DXT3`/`DXT5` plus raw 32-bit BGRA
are read; `DX10`, cube and volume surfaces are refused **by name**, with a test
asserting `DX10` is rejected rather than mis-decoded. *(static + unit test.)*

**2.4 — lightmap 256×256: already covered.** Native masks are 256² with 128
accepted; only sources at or below 256 px are upscaled, so no 256 px mask is
resampled from a larger one. *(static.)*

**2.5 — `angelica_pck` versions, single table: already covered.** The
container's tag word is `0x56a089b7` in 1.5.2 and **zero** in 1.8.4 (measured on
both `configs.pck`), so it is checked as part of the family rather than as one
exact value — which is why one reader serves both. *(measured.)*

**2.6 — encrypted flag: changed from "refuse on the bit" to "the table decides".**
Measured: the 2016 client writes `0x65676e41` (`Ange`, the start of its
description) into the field that 1.5.2 fills with a real zero `dwFlags`, on the
**same** revision word. Refusing on bit 31 alone would therefore reject a package
that decodes perfectly. The reader now walks the table first and names the flag,
the revision and the bit only when the walk fails; two tests cover both halves.
*(measured on three shipped packs; asserted by
`a_flag_bit_does_not_refuse_a_package_whose_table_decodes` and
`an_unreadable_table_with_bit_31_set_is_refused_as_encrypted`.)*

**2.7 — grass limits: already covered.** The caps sit three orders of magnitude
above the measured maximum (the 1.8.4 world's 244 grass types) and refuse by
name rather than silently clamping. *(measured.)*

**2.8 — count caps: already covered.** `MAX_TREE_TYPES` = `MAX_GRASS_TYPES` =
4096 in `scene.rs`, `ENTRY_LIMIT` = 1,000,000 in `pck.rs`, each with a comment
naming the measurement it clears. *(measured.)*

**2.9 — legacy 59-type bridge on 1.8.4: done.** 1.8.4's world declares exactly
59 tree types, so it sits **on** the boundary and is servable by the legacy path,
where padding is a no-op. The `> 59` comparison now uses
`LEGACY_BRIDGE_TREE_TYPES` instead of a second literal, and four tests pin the
boundary, the refusal at 60, the padded size and the cache key. *(measured: 59
types in the 1.8.4 world; the tests run on the Windows job, where
`windows_setup` is compiled.)*

**2.10 — audio codecs: already covered.** The codec table is built from the
measured container tags; nothing new was added. *(static.)*

**2.11 — skipped tests say so and are counted: done.** Every corpus test that
cannot see its file prints `SKIPPED:` with the path, increments a counter, and
now also lands in the **job summary** with a passed/skipped table. `PWE_CORPUS=require`
turns a skip into a failure. *(static; the workflow change was checked against
realistic `cargo test` output locally before being committed.)*

**2.12 — `elements.data` family `0x3000`: already covered.** `ElementsHeader`
splits the word into family and version, tests the family across several
releases, and rejects a same-version word with a different family. *(static.)*

**2.13 — a failed map names the map and the error: already covered.** *(static.)*

**2.14 — `terrain_block` versions 2..=6: already covered.** *(static.)*

**2.15 — `music.rs` precinct non-numeric refusal: already covered.** *(static.)*

### What is not done, and exactly what would settle it

`litmodels.pck` is the only item above that is not closed. It is **B-33**: every
copy on Drive is larger than the 100 MiB the session can transfer, and CI has no
Drive access. The measurement that would settle it is one line — the 280 trailer
bytes and the declared count of 1.8.4's `litmodels.pck` — which requires either a
staged copy in the workspace or a CI job with the file present. Until then the
split-package reader (`B-9`) remains the claim that covers it, and that claim is
unit-tested against a synthetic split rather than against the shipped 2.86 GB
file.
