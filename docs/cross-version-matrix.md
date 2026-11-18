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
