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

## Notes taken while working
