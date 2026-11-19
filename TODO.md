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

- [ ] **R2. Decode the precinct ambience-priority field (header index 3).**
      Five values across the 1.8.4 file (0×53, 1×38, 2×22, 3×104, 4×167),
      correlated with having a stereo ambience path.
      *Done when:* the client's precinct reader is found and the field's use is
      either decoded and implemented, or the search is recorded with what was
      ruled out.

- [ ] **R3. Search `VS_VERSIONINFO` StringTables by language, not first-wins.**
      Concrete, testable, no assets needed beyond `sdk/*.dll` and the staged
      executables.
      *Done when:* the reader prefers a known language (English, then any) and a
      test covers a multi-table resource.

- [ ] **R4. Make the soft/duplicated rows tell the truth.**
      `World/asset gates outside 1.4.2` still says "GFX roots ≤79", which the
      version-ceiling work made false. `Audio codec coverage` and
      `Movement sound table` are written as gaps but are answered. `Instance
      aliases for b07-b29` explains itself but reads like an open defect.
      *Done when:* each row states its current status and the register has no
      claim the code contradicts.

- [ ] **R5. Full gate sweep, push, refresh the PR body.**

## Notes taken while working
