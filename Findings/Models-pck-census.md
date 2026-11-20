# `models.pck` census — what the character/effect pack actually holds

Source: `models.pck`, 888,028,116 B, dated 2019-05-03 (the build matching this
client's `shaders.pck`), pack version `0x20002`. 40,753 members.
Reproduce with `python3 tools/archive-census.py google_drive/models.pck`.

## Correction to a long-standing assumption

This pack does **not** contain the character/effect BMDs. It contains **zero
`.bmd` members**. Every "`models.pck` was never decoded" note in the Phase-1 and
Phase-3 reports is true but was pointing at the wrong thing: BMDs live in
`litmodels.pck` (28,858 world BMDs, censused in
`Phase-3-Agent-A-bmd-corpus-census.md`) and `building.pck` holds no BMDs either.

`models.pck` is the **Angelica EC/ECM** asset pack:

| extension | count | container | parser |
|---|---:|---|---|
| `.stck` | 14,065 | `MOXBKCTS` animation clip | `src/animation.rs` |
| `.ski` | 10,898 | `MOXBIKSA` skin weights | `src/skin.rs` |
| `.dds` | 6,005 | texture | `src/dxt.rs` |
| `.ecm` | 3,745 | text `MOXTVersion: N` | `src/ecmodel.rs` |
| `.bon` | 3,579 | `MOXBEKSA` skeleton | `src/ecmodel.rs` |
| `.smd` | 2,228 | `MOXBDMSA` skin model | `src/ecmodel.rs` |

`.ecm` is a **text** format, not a binary container: `MOXTVersion: 21` followed by
INI-style keys. The pack carries **24 distinct versions**, v12 through v47, and
all 3,745 files parse:

```
v12 5   v13 76   v14 11   v15 2    v18 249  v19 552  v20 40   v21 1547
v22 320 v23 1    v27 13   v29 1    v32 136  v33 136  v34 2    v35 8
v37 6   v39 99   v40 39   v42 289  v43 109  v45 20   v46 3    v47 81
```

## Totals

| verdict | members |
|---|---:|
| supported | 40,569 |
| out of scope | 177 |
| unsupported | **7** |

The 7 are all **zero-byte** authoring leftovers — `logs/af.log`,
`logs/skeletoneditor.log`, `log.txt`, `~ps49.tmp` and two more. Nothing is
skipped for unknown content.

Out-of-scope: `.mphy` physics blobs (101), `.his` build history (12), `.log`
(56), `.rar` archives shipped inside the pack (3), `.ecp` collision hulls (2),
`.bip` authoring source (1), `.dll` (1), a `.tmp` (1).

## Reference integrity

`ECM -> SkinModelPath` resolves for **3,742 of 3,745** references. The three that
do not are an authored path mismatch, not missing content: the ECM points at
e.g. `.../岩灵战士单手短.smd` while the archive carries the whole asset set under
`.../岩灵战士单手短1/`. The `.smd`, `.bon`, `.ski` and `.ecm` are all present — the
model just lives one directory over. Worth knowing because a strict resolver
will refuse to draw those three weapons; a fallback that retries with the
trailing `1` would recover them.

## A bug this census had, that it then found

The first run reported `stck`, `ski`, `bon` and `smd` as unsupported. Cause:
`sniff()` tested the 4-byte `MOXB` prefix *before* the 8-byte container tags, and
since every Angelica binary begins with `MOXB`, the coarse test swallowed all
four specific ones. Longest-match-first fixes it.

This is the same class of error as the extension-level assumptions the census
exists to replace — a coarser test placed ahead of a finer one silently absorbs
every specific case rather than failing loudly. It is recorded here rather than
quietly fixed because the tool reported plausible-looking output while being
wrong, which is the failure mode that matters.

## What this does and does not settle

It settles member-level coverage for this pack: every one of 40,753 members is
named and classified. It does **not** settle BMD mesh-version coverage — see
P3.01, where the remaining gap is mesh versions `0x10000002`/`0x10000003`,
whose layouts are recovered from the client but have zero occurrences in any
staged corpus.
