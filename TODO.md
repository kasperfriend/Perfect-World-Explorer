# To-do — remaining open register entries

Built by reading all 62 rows of `Findings/Unsupported-client-paths.md` and
splitting them into: already resolved but worded softly (bookkeeping), genuinely
implementable here (code), and blocked on something this environment cannot
supply (no GPU / no staged asset / not enough disassembly). Every item is worked
in order; blocked ones are made to say precisely *what* blocks them instead of a
soft "not established".

- [x] **R1. Separate the T2MK `type` byte from the projection axis.**
      The register says `world.rs` reads a `+12` selector (0/1/2) as the
      projection `axis`, and the two meanings have never been separated.
      *Done:* disassembled the real 1.4.2 `elementclient.exe` (6,000,640 B,
      `.text` at `0x401000`). `0x77f180` **never reads** the byte — it is a
      texture-cache acquire that tests `+0x178` and dereferences `[esi]`. A
      whole-image scan finds `movzx` of `+0xc` exactly once (`0x8213e1`), in a
      hash loop. So the byte is renderer-visible but unbranched-on in this image.
      `material::Layer` now carries `selector` (authored, untouched, readable)
      apart from `axis` (the shader projection, derived 1:1), refuses a selector
      outside 0..=2, and two tests pin both the mapping and the accessor.
      **The byte's own native meaning stays open** — no consumer exists to decode,
      and the row now says exactly that instead of "the two have not been
      separated".

- [x] **R2. Decode the precinct ambience-priority field (header index 3).**
      Five values across the 1.8.4 file (0×53, 1×38, 2×22, 3×104, 4×167),
      correlated with having a stereo ambience path.
      *Done:* the reader **was** found — `0x654320` is `CELPrecinct::Load`, it
      parses zones with repeated `seek/read` pairs and stores numbers at `+0x40`,
      `+0x54`, `+0x58`, `+0x68`; the selector `0x654cb0` filters on `+0x54`
      equalling a caller-supplied player state and tie-breaks on `+0x58`. What
      is **not** done is the last step: mapping a file index onto those slots.
      The parse loop is stdcall with a self-balancing stack, so static offsets
      cannot be assigned to indices without modelling each callee's arity, and
      the data is ambiguous (index 8 is 1 for all 383 zones, so a group
      tie-break on it would never fire). Implementing either reading would have
      been a guess, so the register row now names the function, the addresses
      and the missing step instead. No code changed.

- [x] **R3. Search `VS_VERSIONINFO` StringTables by language, not first-wins.**
      Concrete, testable, no assets needed beyond `sdk/*.dll` and the staged
      executables.
      *Done:* `version_string` prefers English (`0409`, `0809`, `0c09`, `1009`)
      ahead of any other table, then falls back to the rest in file order; a
      flattened blob with no enumerable `StringTable` still uses the unfiltered
      walk. `the_english_string_table_wins_over_one_that_precedes_it` authors a
      Chinese table ahead of an English one. `client` gate: 49 passed (was 48).

- [x] **R4. Make the soft/duplicated rows tell the truth.**
      `World/asset gates outside 1.4.2` still says "GFX roots ≤79", which the
      version-ceiling work made false. `Audio codec coverage` and
      `Movement sound table` are written as gaps but are answered. `Instance
      aliases for b07-b29` explains itself but reads like an open defect.
      *Done:* four rows rewritten. `World/asset gates outside 1.4.2` no longer
      claims a GFX ceiling of 79 — it names `gfx_file::MAX_VERSION = 103` and
      says an over-ceiling root is read and reported, not refused.
      `Audio codec coverage` and `Movement sound table` now open by saying they
      are answered. `Instance aliases for b07-b29` is stated as an absent input
      rather than an open defect, and the regenerating tool is named.

- [x] **R5. Full gate sweep, push, refresh the PR body.**
      All fifteen gates green: `check`, `test` 211, `diagnostics` 18, `music` 27,
      `player` 286, `gfx` 94, `water` 20, `material` 27, `bins` 38/38, `scene`
      3 ok/0 failed, `pck` 3 ok/0 failed, `content` 2828 read/1 failed (the known
      4-byte `MOXT` stub, `翼击击中.gfx`), `matrix` ok, `matrix-maps` ok,
      `instance` 2 maps/0 failed. One gate was itself broken and is fixed here:
      `matrix` had stopped compiling because `src/water.rs` reports through
      `crate::diag!` and that gate had no stub for it.

## Second pass — the CI streak, and the numbers nobody re-measured

R1–R5 were closed, but the branch was still red. Working the CI failures to
green uncovered a second class of defect: **checks that named a thing without
being that thing**, and **measurements of files this checkout does not have**.

- [x] **R6. Get CI green after B-12.** Four failures, each hiding the next:
      modules a GPU check declares by hand (`effect_census`, `sky_time`,
      `loading_graph`), a struct field two hand-built fixtures omitted
      (`Trees::off_reason`), a walker-audio suite no local mode compiled (it had
      been asserting the wrong event ids since it was written), and three
      unformatted files the `fmt` mode never looked at. Fixed by deriving each
      check's scope from the source instead of restating it:
      `tools/binary-module-closure.py` and `tools/binary-field-closure.py`
      follow each binary's `#[path]` graph; `fmt` walks `src/**/*.rs` with
      `find` instead of twenty named modules; `player` compiles the walker's
      audio module. First green run since `6a8f2ec`: Linux 2m45s, Windows 7m38s.

- [x] **R7. The fixture that was a paraphrase.** `SHIPPED_ROWS` held 11 of the
      real `configs/sound.txt`'s 67 rows and was missing 105, the id the rule
      produces for a walking female Psychic, so a real lookup returned `None`
      and the test unwrapped it. The fixture is now the member decoded out of
      the staged `configs.pck`, and three assertions changed to match the client:
      length 67, the table's own path casing, and the no-table fallback id 100
      (106 is a different, silent slot).

- [x] **R8. "The 1.8.4 precinct is 383 zones, 783 tracks, 347 ambience paths"**
      is true of the **world** map's 885 KB file and false of the 78,858 B one
      this checkout stages (29 zones, 29 tracks, no ambience). Both are now
      measured by `src/bin/precinct-check.rs`, which runs in CI and in
      `tools/offline_harness.sh precinct`. Three register rows corrected against
      it; `Native audio and environmental sound` no longer contradicts
      `Audio codec coverage`; `Instance aliases` said 68 rows/65 folders where
      the generator reports 65/60.

- [x] **R9. A skip that was three skips, unnamed.** One malformed precinct zone
      was reported as three, all as `Precinct zone "":` -- the zone with a real
      name was never mentioned. `Zones` now carries `malformed`, the walker
      resumes at the next line that looks like a zone, and the test pins the
      count instead of ignoring it. `instance` printed `outside 0` beside
      `extent no trn2`, a pass for a check that could not run; it prints `--`.

### Still open, and why each one is

The register's remaining rows are blocked on inputs or evidence this
environment cannot supply, and each now says which:

| row | what would close it |
|---|---|
| `Native terrain level thresholds` | more of the 1.4.2 `.text` than the staged 6,000,640 B image exposes at those addresses |
| `Precinct ambience priority` | modelling each callee's arity in the stdcall parse loop, or a build whose index 8 is not constant |
| `Precinct ambience randomisation` | client evidence for a crossfade; the format offers no second variant to pick |
| `Complete fog variants`, `Terrain layer type byte`, `Controller-108 LUT` | a consumer in the disassembly, or authored data that varies |
| `Visual GFX graph`, `Every effect`, `Whole-world GPU residency` | a GPU, and the 40+ GB effect corpus |
| `elements.data` beyond v27, `tasks.data` beyond v75 | a client shipping a newer revision |
| `Native audio ... physical-device behavior` | the client's device enumeration, which is not in the staged code |

Do not close a row by rewording it. The ones closed above were closed by
implementing behaviour (R7, R9) or by measuring the file the claim was about
(R8).

## Notes taken while working
