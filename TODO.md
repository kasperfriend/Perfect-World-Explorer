# To-do — branch `arena/3074ae8a-perfect-world-explorer`

Ordered, each item has a definition of done. Ticked = done and verified.
No item is closed on an assumption; where the evidence is not on disk, the
item says so and lands as a named gap instead of a guess.

- [x] **1. Resolve the TRN2 / precinct coordinate frame** (the one entry
      `Findings/Unsupported-client-paths.md` still calls *open*).
      Precinct polygons reach z = 5,637 and negative x in a map whose `.trn2`
      and `.ecwld` both declare 6,144 × 3,072. Decide between *shared offset
      frame* and *centred authoring frame* using the ECWLD's own placements
      (water, ornaments, effects) as the reference cloud — not by eye.
      *Done:* **Resolved, the frame is centred on the origin.** a37's own
      `precinct.clt` has 29 zones, all 29 inside the centred 6,144 × 3,072 box,
      0 outside, x -1,936..651 — and -1,936 is exactly the min x of a37's 362
      ECWLD placements. `world.ecwld`'s 7,737 placements fill the centred
      8,192 × 11,264 box to 99.5 %/97.5 % with 0 outside.
      **Two things were wrong and are now fixed:** `contains_world_point` tested
      `0.0..=width` (corner-anchored); and the "383-zone a37 precinct" I staged
      last stretch was byte-identical to the *world* file — under that mix-up
      240 of 383 centres fell outside a37's extent, which is what exposed it.
      `instance` now prints an `outside` column.

- [x] **2. Stage `sfx.pck` and settle whether element-170 paths resolve.**
      `world_audio` resolves under an `sfx/` prefix; the 355 authored 170 paths
      are `Skill\…`, `Effect\…`, `Creature\…`, `Item\…`, `Ambience\…`.
      *Done:* the **key rule is settled** — `configs/sound.txt` writes
      `SFX\Item\EquipSound\DownGem.wav`, a 170 record writes
      `Item\EquipSound\DownGem.wav`, and both key to
      `sfx/item/equipsound/downgem.wav` (`src/sound_key.rs`, 4 tests).
      **`sfx.pck` is not obtainable**: all nine copies are 123–880 MB, over the
      100 MiB transfer limit. So the gap is "no staged archive", not "unknown
      path base", and B-31 now says exactly that.

- [ ] **3. Run the silent-failure guard in CI, not just locally.**
      `tools/check-harness.py` currently runs only inside `harness.sh check`.
      *Done when:* the workflow runs it on both platforms and a deliberate
      duplicate label fails the build (checked, then reverted).

- [ ] **4. Re-read the two findings docs for claims this stretch invalidated.**
      `Findings/Unsupported-client-paths.md` and `docs/blockers.md` both carry
      older wording ("counts but does not perform", the 79-version ceiling, the
      precinct frame).
      *Done when:* every row this stretch touched states the current truth, and
      nothing still claims 160/190 are unimplemented.

- [ ] **5. Close out: full gate sweep, benchmark the numbers, push, refresh the
      PR body.**
      *Done when:* every mode is green at HEAD, the tree is clean, branch and
      origin match, and the PR body carries the measured numbers.

## Notes taken while working

- **A staged file can be the wrong map's file.** `a37/precinct.clt` was the
  world's, byte for byte. Nothing about parsing it failed; only comparing it
  against the map it sits beside did. Any "N of M read" number is only as
  trustworthy as the join between a file and its map.
