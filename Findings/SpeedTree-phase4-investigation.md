# SpeedTree Phase 4 investigation (partial)

This is an asset inventory and emulation/static-disassembly investigation,
**not** an implementation of P4.01–P4.03. The existing viewer still uses
approximate LOD transition widths and sampled leaf motion. The companion
`SpeedTree-world-asset-survey.tsv` inventories all 59 *world type records*.
`SpeedTree-emulated-materials.jsonl` records DLL material getter results and
raw transition-related object fields for all 59 after emulated SPT loading;
these are experimental and are not a survey of client-applied material state,
generated LOD geometry, billboards, or animation.

## Reproduction and evidence

Using the client files from Drive, whose hashes identify the binaries studied:

| file | SHA-256 |
|---|---|
| elementclient.exe | a290998dc13cc5558b54bda3f65d099f95bc0702e54a0613c6d7669f35c6cdb0 |
| speedtreert.dll | 7f0a401688f63c520ab717a4b9939705e5aac635a649a8cfb5df6af19681b46f |
| trees.pck | 6cbe043e75f9c249c3720f986e7424a3cf8cd4727c5d0714549e2bdb901247f8 |
| shaders.pck | 34a556cc2f69f54f52dc244d4d4249350cb99b81218a98c7dc3362acf092061b |

The `world.ecwld` MD5 is `eb5e4c7be58548341dc3cabf1359949e`. PCK
records in the TSV are hashed after decompression. Run:

```sh
python3 tools/speedtree/asset_survey.py google_drive/world.ecwld google_drive/trees.pck > Findings/SpeedTree-world-asset-survey.tsv
python3 -m unittest discover -s tools/speedtree -p 'test_asset_survey.py' -v
objdump -d -Mintel --start-address=0x656270 --stop-address=0x6562e0 google_drive/elementclient.exe
objdump -d -Mintel --start-address=0x100282b0 --stop-address=0x10028380 google_drive/speedtreert.dll
```

The inventory reports 59 rows, 57 distinct SPT content hashes and 37 distinct
composite content hashes; it verifies each referenced PCK record exists and
its decompressed length matches the directory. Paths preserve the ECWLD type
order, which is the cache type ID. This is not an enumeration of instances.

## Experimental material getter survey

A follow-up executes DLL initialization, constructor, `LoadTree`, and the
branch, frond and leaf material getters for each world SPT under Speakeasy /
Unicorn. The resulting `SpeedTree-emulated-materials.jsonl` has 59 records;
all processes returned success and duplicate SPT hashes yielded identical
arrays. A second full run of all 59 reproduced the original material arrays.
The raw fields `+0x18`, `+0x1c`, `+0x20`, `+0x24`, `+0x28` and `+0x44`
show 24 distinct tuples; 58 types have mode 0 and type 39 (`Palm1.spt`)
has mode 1. At `GetLeafGeometry` `0x10024e74`, mode 1 enters the two-leaf
`GetTransitionValues` path; mode 0 follows the discrete-leaf path at
`0x10024fce`, which disables the second leaf set at `0x1002501a`. These are
post-LoadTree emulated fields and static branch analysis, not a complete
end-to-end client rendering test. Three of these floats are populated by `ParseLodInfo` at `0x10022e1c`, `0x10022e28`
and `0x10022e34`. They are *inputs*, not independently measured LOD bounds
or proof of component geometry. The 13 raw floats contain 5, 11 and 13 distinct branch, frond and
leaf arrays respectively. Some leaves have non-zero values in slots 6–8,
9–11, or 12, so assuming one uniform material is not justified.

This is **not verified Windows output**: the emulator substitutes CRT
allocation, string scanning, `_ftol`, `atof`, trigonometric and pow routines,
uses fake file-descriptor responses, and explicitly runs the DLL's initializer
array because Speakeasy's `_initterm` is a no-op. Only SPT loading and getters
are measured, not Compute or the renderer, and array slots have not been
independently mapped to client-applied material state. Do not use these raw
values as a production fix without validating ABI, client call paths and the
substitutes against an independent native run. For example, an experimental
Compute attempt produced a questionable type-0 height and has deliberately
not been used as evidence.

To reproduce (requires `pefile`, `speakeasy-emulator` and `unicorn`):

```sh
mkdir -p google_drive/research/materials
seq 0 58 | xargs -P 4 -I '{}' sh -c 'PYTHONPATH=.cache/py python3 tools/speedtree/probe_materials_emulated.py google_drive/world.ecwld google_drive/trees.pck google_drive/speedtreert.dll --type "$1" > "google_drive/research/materials/$1.json" 2> "google_drive/research/materials/$1.err"' _ '{}'
```

Result: exit 0, 59 JSON files, no stderr; an additional full 59-type run
with the corrected `_CIpow` substitute reproduced the recorded material and
raw LOD fields exactly. `PYTHONPATH=.cache/py python3 -m unittest discover
-s tools/speedtree -p 'test*.py' -v` passed seven tests, including ordinary
finite-input checks of the substituted x87 arithmetic. Those tests do not
validate MSVCRT edge cases or geometry. World SHA-256 is
`6a763c0ea0ac5717adf0a806cc2d2c09e86a4e4a809df60fc8ed0f9a5c319fc8`.

## Static observations

* Client `0x656270–0x6562c8` retrieves the computed bounding box, takes
  `max(max_z - min_z, 5.0)`, multiplies it by the owner field `+0x228`, then
  calls `SetLodLimits` with near = that product × 5 and far = that product ×
  16. The operands at `0x8d5ee8` and `0x8d7320` decode as 5 and 16.
  A second call site is `0x657d92`. The meaning and update path of owner
  `+0x228` still need end-to-end validation; it must not be equated blindly
  with viewer quality settings.
* `CSpeedTreeRT::ComputeLodLevel` export `0x10022050` delegates to
  `0x100282b0`, which takes all three components of the camera-relative
  vector and computes an approximate square root using the float-bit operation
  `(bits(distance_squared) >> 1) + 0x1fc00000`, before applying near/far
  limits and clamping. Thus the viewer's XZ-only distance and ordinary
  Euclidean sqrt would not be exact. NaN/zero and instruction-level x87
  behavior remain unvalidated against a running DLL.
* Follow-up: `PYTHONPATH=.cache/py python3 tools/speedtree/probe_lod_unicorn.py google_drive/speedtreert.dll` executed that isolated DLL routine under Unicorn (not Wine or Windows) with near=50, far=160. It returned 1 at distance 25, 0.986079574 at 50, 0.517613649 at 100, and 0 at 160. A vertical-only displacement and a translated-camera case produced the same result as the corresponding horizontal relative displacement. This confirms the approximate-sqrt behavior in this emulator; the boundary at exactly the nominal near limit is already below 1. The script checks the exact DLL SHA-256 and uses `pefile` and `unicorn` Python packages. These fixtures are not independent native comparison and do not determine component transitions.
* `GetDiscreteBranchLodLevel` (`0x10022260`) and
  `GetDiscreteFrondLodLevel` (`0x100222b0`) depend on continuous level and
  the runtime's respective LOD counts; `GetDiscreteLeafLodLevel`
  (`0x10022300`) has a separate drop-to-billboard branch. This is not yet a
  proof of alpha transitions, hysteresis or what the client actually draws.
  `GetTransitionValues` is at `0x100254f0`, `GetGeometry` at `0x10024680`.
  `PYTHONPATH=.cache/py python3 tools/speedtree/probe_transitions_unicorn.py
  google_drive/speedtreert.dll` checks ten synthetic transition cases using
  three levels, width 0.07, bias 0.2, exponent 1.5 and alpha reference 84.
  For example, at continuous level 0.736 the export returns LODs 0 and 1
  with alpha thresholds 84 and 253.475; at 0.596 it returns LOD 1 alone.
  This is alpha-threshold evolution, not evidence for the viewer's ordered
  dither crossfade. These fixtures use substituted CRT `_ftol` and `_CIpow`;
  the pow stub was corrected for zero-base, positive-exponent inputs after
  an initial implementation produced invalid values at transition endpoints.
  Only ordinary finite inputs are tested, and the transition call sites and
  actual generated geometry counts still require end-to-end validation.
  The isolated getters were subsequently emulated using synthetic structures:
  `PYTHONPATH=.cache/py python3 tools/speedtree/probe_discrete_lod_unicorn.py google_drive/speedtreert.dll`.
  For three levels with drop-to-billboard disabled, branch, frond and leaf
  produce indices 0 at level 0.7, 1 at 0.6, and 2 at 0.3. With leaf
  drop-to-billboard enabled **and** a positive billboard count, the leaf
  getter uses an extra slot: level 0.25 and 0 return index 3; clearing either
  prerequisite restores index 2 at 0.25. The probe substitutes a small x87
  `_ftol` CRT routine and has not been independently checked on Windows.
  An index 3 here is not proof the client renders a billboard: the client
  still needs to be traced through its visibility and draw paths. At client
  `0x656084`, EDI is zeroed and is not reassigned before the call to
  `SetDropToBillboard` at `0x6562db`, so this initialization path passes
  false (subject to normal x86 callee-saved register conventions). The two
  branch, leaf and frond lighting setters at `0x6561b4`, `0x6561c0`,
  `0x6561cc` receive the same zero. The full draw-path and any
  later state changes remain to be traced.
* At `0x6561d9`, `0x6561e6`, `0x6561f3` the client sets all three wind
  methods to enum 2, and at `0x656200` sets one leaf rocking group. This
  alone does not prove that branch/frond vertices move at runtime. The
  manager calls `SetTime` at `0x650080`; instance update calls
  `ComputeLodLevel` at `0x6569fc` and `SetWindStrength` around `0x656a1e`.
* The `shaders.pck` records `shaders/vs/treebranchvs.txt` and
  `shaders/ps/treebranch.txt` describe a tangent/binormal normal-mapped
  branch path with directional and ambient terms, `mul_x2`, and a fourth
  power specular term with constant `(0.5, 0.5, 0.5, 1)`. Their presence
  does not establish when the client activates that shader. The client
  imports lighting-method setters, not the DLL material getters; material
  state and the other rendering lanes require further tracing.

## Remaining work / limitations

No production tree path was changed: incomplete static evidence is not a
safe basis for replacing the current approximations or claiming exactness.
The PWTREE07 cache has only three geometry snapshots and sampled motion; exact
runtime LOD and wind require additional data and/or a runtime implementation.
DLL SPT loading and isolated LOD routines were emulated with CRT substitutes,
but the client renderer and reliable computed geometry were not validated. The
split texture archive was not extracted, and no visual or GPU comparison was
made. The environment had no Rust toolchain,
Wine or Windows cross compiler, so Rust builds and native probes were not run.
A native 32-bit client/DLL probe or a verified emulator should capture every
type's material arrays, actual geometry, alpha, LOD transition behavior and
wind at multiple times and strengths before production changes and material,
LOD-boundary and animation regressions can be accepted.

### Continuous-level translation and additional tooling (2026-11-03)

- `src/tree_lod.rs::client_level` is an **experimental, not yet used in rendering** translation of the isolated `speedtreert.dll:0x100282b0` calculation. It uses 3D distance, rounds the squared distance to f32 and applies the DLL's bit-based distance estimate. Its f64 intermediates are not guaranteed to match x87 extended precision at every rounding boundary; invalid or overflowed inputs return `None`. It takes already-determined near and far limits and does not provide component LODs, crossfades, billboard policy, or geometry. The existing approximate snapshot selection remains in use; P4.02 remains **No**. Integrating this function prematurely would not make the old cache's three snapshots equivalent to client geometry.
- `PATH="$PWD/.cache/rust/bin:$PATH" rustc --edition=2021 --test src/tree_lod.rs -o .cache/tree-lod-tests && .cache/tree-lod-tests`: 3 tests passed, including emulated-DLL fixtures. `PATH="$PWD/.cache/rust/bin:$PATH" PYTHONPATH=.cache/py python3 tools/speedtree/compare_lod_rust.py google_drive/speedtreert.dll`: 2,000 seeded finite cases bitwise matched the DLL routine in Unicorn. Neither is a native Windows/client comparison, and random agreement does not prove all inputs.
- The two supplied texture RAR volumes were extracted with npm `node-unrar-js` 2.0.2 (`createExtractorFromFile` on `google_drive/textures.part1.rar`; fully consume `extract({}).files`). The resulting ignored `google_drive/textures.pck` MD5 is `e6d48bb269d5f7b5ff576fe1045fdca9`, matching the Drive record. This makes the PCK available for further material/texture tracing; its extraction alone does not validate material state.
- Rust 1.88.0 was obtained from npm `@rustbin/rustc-1.88.0-x86_64-unknown-linux-gnu`, `@rustbin/rust-std-1.88.0-x86_64-unknown-linux-gnu`, and `@rustbin/cargo-1.88.0-x86_64-unknown-linux-gnu` and unpacked under ignored `.cache/rust`. Full `PATH="$PWD/.cache/rust/bin:$PATH" CARGO_HOME="$PWD/.cache/cargo" CARGO_NET_RETRY=0 CARGO_HTTP_TIMEOUT=10 cargo test --locked --no-default-features --lib tree_lod::tests` failed because `https://index.crates.io/config.json` could not establish TLS; offline mode failed because `egui` was not cached. The direct standalone Rust test above does not establish that the whole project builds.

### Client tree wind-strength signal (2026-11-03, no visual-comparison requirement)

The user's revised acceptance allows code tracing and automated comparisons in place of visual comparisons. It does **not** make the current sampled-leaf implementation equivalent to the game; P4.03 remains **No** until the DLL's full live leaf calculation and its inputs are implemented and verified. P4.01 and P4.02 also remain **No**.

The client initializes its tree-strength noise at `elementclient.exe:0x64fcbe–0x64fcee`: 256 samples, scalar amplitude 1, initial period 256, persistence bit pattern `0x3f7fbe77`, eight octaves and seed `0x12345`. The routine at `0x766320` generates three channels with the Park–Miller recurrence `(seed * 16807) % 2147483647`, signed sample `(seed % 2001 - 1000) * float32(0.001)`, and neighbor smoothing `(previous + next) / 4 + current / 2`. It then configures octave offsets and normalized weights at `0x766940`; periods are 256,128,...,2 and offsets are 215,370,417,434,952,739,9,779. `0x766490` interpolates the periodic table, takes the absolute value, and adds the weighted octaves. For this configuration the longest repeated interval of the scalar signal is 65536 seconds, **not** 16 seconds.

The manager at `0x650080` accumulates its f32 clock from unsigned millisecond increments multiplied by the f32 constant at `0x8d5ef8` (`0.001`); `0x6506b0` samples the noise using this same manager clock. Although the caller at `0x656a06–0x656a1e` passes an instance-position pointer into `0x6506b0`, that function does not read it. In this traced path all instances receive the same time-derived strength for a given manager tick. This does **not** prove all leaf positions, wind phases, loading orders, or alternate paths are shared. The per-visible-instance DLL call is `SetWindStrength(strength, -1, -1)` after `ComputeLodLevel` and before leaf `GetGeometry`.

`src/tree_wind.rs` contains an experimental translation of this scalar signal only; it is deliberately **not wired into rendering** because scaling the existing 16-second samples by it would still be an approximation, and the cache lacks the data required to reconstruct the DLL's actual rocking. `tools/speedtree/probe_client_wind.py` emulates the client instructions using only allocator and `_ftol` substitutions; `tools/speedtree/compare_wind_rust.py` compares the translation with 2,006 deterministic samples. This script explicitly sets x87 control word `0x027f` (conventional MSVC CRT 53-bit precision) because Unicorn's default is `0x0000` (24-bit precision); the actual client runtime control word is not yet captured. On this assumed configuration the maximum absolute error was `4.999e-10`. At `t=0` strength is approximately `0.386193573`, at `t=16` approximately `0.356702805`, and at `t=1024` approximately `0.194191948`. These are isolated-code emulation results, not a claim of end-to-end equivalence.

Commands/results:

- `PATH="$PWD/.cache/rust/bin:$PATH" rustc --edition=2021 --test src/tree_wind.rs -o .cache/tree-wind-tests && .cache/tree-wind-tests`: 1 passed.
- `PATH="$PWD/.cache/rust/bin:$PATH" PYTHONPATH=.cache/py python3 tools/speedtree/compare_wind_rust.py google_drive/elementclient.exe`: 2,006 cases within `1e-6`.
- `PYTHONPATH=.cache/py python3 -m unittest discover -s tools/speedtree -p 'test_*.py'`: 9 passed (requires Unicorn 1.0.2 for Speakeasy compatibility).

Earlier DLL probes did not explicitly set the Unicorn x87 control word, so their default `0x0000` should not be assumed to represent the native client. A 3,000-input exploratory comparison of the isolated continuous-LOD routine under `0x0000` and `0x027f` found no differences, but that does not establish equivalence for all inputs or for material, transition, and wind routines. Those results retain their stated emulation-only limitations.

### Geometry count probe and strict baker checks (2026-11-03, continued)

An optional `--compute-lod-counts` flag was added to `probe_materials_emulated.py`. After the existing LoadTree path it applies the baker's lighting/wind settings, one rocking group, source size times 0.3048, and Compute arguments `(null, 1, true)`, then calls the three `GetNum*LodLevels` DLL exports. This is **experimental emulation**, not evidence that generated geometry or size is correct: the previously observed questionable bounding box, emulated CRT and PRNG are unresolved. An initial exploratory run did not check for interrupted emulation and its counts are superseded by the completion-checked survey below. Even the latter is not a native count survey or geometry validation. Reproduction for type 12:

```
PYTHONPATH=.cache/py python3 tools/speedtree/probe_materials_emulated.py google_drive/world.ecwld google_drive/trees.pck google_drive/speedtreert.dll --type 12 --compute-lod-counts
```

The **source** for the Windows baker now rejects missing/nonfinite branch or frond normals with a tree type and material in its error instead of fabricating an up normal. It also rejects the previously silent fixed-camera leaf fallback and invalid rocking camera transform. This intentionally makes unsupported data fail rather than silently producing estimated geometry. It does not recover normals, tangents, material constants, or leaf motion, and the existing viewer still uses the old embedded helper `tools/speedtree/speedtree-bake.exe`; that binary has **not** been replaced because the newly cross-compiled helper cannot be run or validated here. Thus these checks are not active in existing viewer builds. Old cache versions also remain accepted. P4.01–P4.03 remain **No**.

Local validation:

- `g++ -std=c++17 -Wall -Wextra -Werror tools/speedtree/test_bake_geometry.cpp -o .cache/test_bake_geometry && .cache/test_bake_geometry`: passed valid strip/serialization and missing/nonfinite-normal, invalid-index and empty-geometry checks.
- `python3 -m pip install --target .cache/zig ziglang==0.13.0` and `.cache/zig/ziglang/zig c++ -target x86-windows-gnu -O2 -static -municode tools/speedtree/bake.cpp -o .cache/speedtree-bake-test.exe`: cross-compile succeeded. The result links UCRT API-set DLLs unlike the existing helper's `msvcrt.dll`, and it has **not** been run on Windows. It is not committed or used as the embedded executable.
- `PYTHONPATH=.cache/py python3 -m unittest discover -s tools/speedtree -p 'test_*.py'`: 9 passed with Unicorn 1.0.2 and Speakeasy.

### Discrete LOD translation and 59-type emulated Compute attempts

`src/tree_lod.rs::client_discrete_level` translates the discrete branch/frond/leaf getters for explicit finite levels in `[0,1]` and counts up to 32767. With the extra leaf billboard slot enabled it returns the slot index as `count`; with no component levels and no billboard it returns `None`. The DLL's negative-level sentinel is not translated. It remains **not wired into the viewer**: selecting a component index is not sufficient while PWTREE07 contains only three combined snapshots, no individual component-level geometry, and no verified transition metadata. No approximate fades have been removed from rendering and P4.02 is still **No**.

`PATH="$PWD/.cache/rust/bin:$PATH" rustc --edition=2021 --test src/tree_lod.rs -o .cache/tree-lod-tests && .cache/tree-lod-tests` passed 4 tests. `PATH="$PWD/.cache/rust/bin:$PATH" PYTHONPATH=.cache/py python3 tools/speedtree/compare_discrete_lod_rust.py google_drive/speedtreert.dll` matched 2,090 synthetic inputs against the emulated DLL getters, with FPCW set to the assumed MSVC default `0x027f` and the substituted `_ftol`. This does not test all cases, generated geometry, transitions, or actual renderer behavior.

The initial parallel survey was unreliable: it did not check whether Speakeasy actually returned from each emulated call before reading EAX. The seven results previously reported as false and the reported component counts may instead have been obtained after interruption. The TSV has since been replaced by the completion-checked survey described below; do not use the initial numbers. The earlier parallel run used:

```
seq 0 58 | xargs -P 6 -I '{}' sh -c 'PYTHONPATH=.cache/py python3 tools/speedtree/probe_materials_emulated.py google_drive/world.ecwld google_drive/trees.pck google_drive/speedtreert.dll --type "$1" --compute-lod-counts > .cache/lod-survey/$1.json 2>.cache/lod-survey/$1.err' sh '{}'
```

P4.01, P4.02 and P4.03 are all within this agent's scope and remain **No**; no visual comparison is required under the user's revised instruction, but executable implementation and sufficient code/data-based verification are still required.

### Transition-function translation and expanded Compute budget

`src/tree_lod.rs::client_transition` translates `speedtreert.dll:0x100254f0` for valid finite parameters and returns up to two component-level indices and their client alpha-reference thresholds in 0–255 units. These thresholds are **cutout thresholds**, not the opacity/ordered-dither fades currently used by the viewer. The function is **not connected to rendering**, because the old cache lacks separate component-level geometry and the full per-type runtime state. It therefore does not complete P4.02.

- `PATH="$PWD/.cache/rust/bin:$PATH" rustc --edition=2021 --test src/tree_lod.rs -o .cache/tree-lod-tests && .cache/tree-lod-tests`: 5 passed, including the earlier 10 transition fixtures.
- `PATH="$PWD/.cache/rust/bin:$PATH" PYTHONPATH=.cache/py python3 tools/speedtree/compare_transition_rust.py google_drive/speedtreert.dll`: 2,077 synthetic inputs matched indices and alpha values within 0.002 units, maximum observed error below `5e-10`; assumes x87 control word `0x027f`, and substitutes CRT `_ftol` and `_CIpow`. This does not prove real client behavior or equivalence for all inputs.
- A type 33 retry with a larger API budget appeared to return `(2,2,2)`, but the old probe did not verify call completion. It has now been superseded by the completion-checked survey below.

### Completion-checked 59-type emulation and production distance (2026-11-03)

Speakeasy's `call` can stop before its return hook without setting the run's error (for example, at its wall-clock timeout). The prior probe interpreted EAX even then; both its false-return reports and some seemingly successful counts were therefore unreliable. The probe now rejects every DLL entry, initializer or export call that did not finish (`run_complete`), with the stop address, and exposes `--timeout` and `--fpcw`. A mock regression checks that stopped calls are rejected, including when EAX might otherwise look like success. The previous TSV has been replaced, not merged with those results.

All 59 world records completed the checked probe with a 300-second per-call budget, a 100,000 API-call limit, and the assumed MSVC x87 control word `0x027f`. `Findings/SpeedTree-emulated-lod-counts.tsv` now contains 53 `(2,2,2)`, three `(1,1,1)`, two `(6,4,3)`, and one `(5,5,6)` branch/frond/leaf tuples. Both duplicate SPT pairs agree. For all 59 records, the pre-Compute material arrays and raw LOD fields match the previously recorded JSONL. Eight prior TSV entries changed: 24, 26, 49, 51, 52, 53, 55, and 56; the seven previously described as Compute-false now complete. Reruns of types 11, 24, 26 and 49 with the default emulated x87 setting produced the same tuples as `0x027f`; this does not identify the exact cause of each old result. Neither the client's actual x87 state nor CRT substitutions, generated bounds, geometry or material state have been validated natively, so these are still emulation observations, not proof of all 59 native geometries.

```
mkdir -p .cache/checked-survey
seq 0 58 | xargs -P 3 -I '{}' sh -c 'PYTHONPATH=.cache/py python3 tools/speedtree/probe_materials_emulated.py google_drive/world.ecwld google_drive/trees.pck google_drive/speedtreert.dll --type "$1" --compute-lod-counts --max-api-count 100000 --fpcw 0x27f --timeout 300 > .cache/checked-survey/$1.json 2>.cache/checked-survey/$1.err; exit $?' sh '{}'
```

The GPU tree submission now uses `tree_lod::client_distance` for the 3D DLL-style bit estimate instead of XZ Euclidean distance; non-finite/overflowed positions are skipped. This is a partial correction, not an implementation of the native component transitions: the old three combined snapshots, estimated heights, fades and billboard policy still remain. `Trees::open` now includes the tree path and branch or frond/leaf material texture in texture read/decode diagnostics, but it does not yet reject every unsupported material combination. P4.01, P4.02 and P4.03 remain **No**.

Checks: the standalone Rust LOD tests pass 6/6; 2,000 seeded isolated DLL distance/level cases match bitwise in Unicorn; the SpeedTree Python tests pass 10/10. Full Cargo testing remains blocked here: offline resolution lacks `egui`, and online crates.io index access fails TLS. The GPU renderer and Windows helper were not run.

### Compatibility-layer execution of the DLL (2026-11-03)

A separate **x86 CPU execution** check is now possible without Windows: wibo `74517de90dc92855571c8fb76941b1051bc41d8e` x86-64 compatibility mode, using locally built Wine-derived CRTs from `encounter/winedll` commit `efdce88535159ae9d527a0c3c47c81778a3860fd`. The wibo build used GCC 12 with a local dependent-static-assert workaround and GNU ld text-base option; the CRT build used Zig 0.13 x86 Windows target, an explicit DLL entry point and patched SafeSEH metadata on three compiler-runtime assembly objects. This is **not Windows**: both the compatibility layer and modified CRT may affect results. These tools and proprietary inputs remain untracked. No visual comparison was performed.

The *existing* embedded baker ran against all 59 SPTs and the selected DLL in this environment and returned `Done: 0 failures`. The independent diagnostic `tools/speedtree/lod-count-probe.cpp` performs LoadTree, the baker's same lighting/wind/group/size setup, Compute and the three count getters, and records bounds. The resulting `Findings/SpeedTree-wibo-lod-counts.tsv` agrees with the completion-checked Speakeasy survey for all 59 records (`python3 tools/speedtree/compare_lod_counts.py Findings/SpeedTree-emulated-lod-counts.tsv Findings/SpeedTree-wibo-lod-counts.tsv`). Type 0 has `max_z=124.927773`, independently reproduced from the earlier emulated `124.92775` under this compatibility environment. This corroborates the observed size but does not establish the client's intended size, scale or bounds behavior.

The stricter source baker also ran under wibo in an instrumented abort-on-error build on each type. It completed 56/59; types 34, 47 and 48 had nonfinite branch normals at vertex 0, and that vertex was referenced by a strip. This is not evidence that Windows generates the same values, and it is not safe to fabricate a normal. The unmodified newly compiled baker could not report the exception under wibo because its C++ unwinder requested the unsupported `K32EnumProcessModules` API. It remains unshipped; this check does not justify replacing the embedded helper. Production P4.01–P4.03 remain incomplete.

Reproduction after preparing `tree-000.spt` through `tree-058.spt` and the described compatibility toolchain:

```
.cache/zig/ziglang/zig c++ -target x86-windows-gnu -O2 -static -municode tools/speedtree/lod-count-probe.cpp -o .cache/lod-count-probe.exe
.cache/wibo-build/wibo .cache/lod-count-probe.exe "$PWD/google_drive/speedtreert.dll" "$PWD/.cache/tree-source" > .cache/wibo-counts.tsv
python3 tools/speedtree/compare_lod_counts.py Findings/SpeedTree-emulated-lod-counts.tsv .cache/wibo-counts.tsv
```

The source baker's nonfinite-normal diagnostic now additionally reports the vertex and vector component. `g++ -std=c++17 -Wall -Wextra -Werror tools/speedtree/test_bake_geometry.cpp -o .cache/test_bake_geometry && .cache/test_bake_geometry` passes, including the specific location in the error; the Zig cross-compile succeeds, but this is still not the embedded helper. The SpeedTree Python suite now passes 12 tests, including matching, missing, duplicate and mismatched LOD survey checks.

### Loaded-geometry LOD traces (2026-11-03)

`tools/speedtree/lod-geometry-probe.cpp` loads and computes an SPT using the baker's settings, calls the actual DLL `GetGeometry` with automatic component selection (`-1` for each short argument), and reports branch/frond/leaf indices, cutout references, and geometry counts for an evenly spaced range of explicit continuous LOD levels. It sets the global `SetDropToBillboard(false)` as observed in client initialization. This is direct DLL execution in the previously described wibo/Wine-derived CRT environment, not an end-to-end client or Windows run.

Using 101 samples for each of 59 world records, 5,959 samples matched the experimental Rust discrete and leaf-transition translations, with maximum leaf alpha error `4.82e-7` (alpha range 0–255). This now compares against `GetGeometry` on loaded, computed real SPTs rather than only synthetic isolated-call fixtures. The sweep found branch and frond alpha reference 84 throughout the sampled levels; 58 mode-0 types had a single leaf set at 84, and mode-1 type 39 had two active leaf sets during some transitions, both with zero cards in these samples. This is a sampled observation, **not** proof of behavior between sample points, alternate states, or other SPTs. In the traced client manager, `0x65015f` enables alpha testing (`0x77a320` writes D3DRS_ALPHATESTENABLE 15) and `0x650174` sets D3DRS_ALPHAFUNC 25 to 5 (`GREATER`); branch and frond pass code sets D3DRS_ALPHAREF 24 via `0x77a380` at `0x656c01` and `0x656d4a` respectively; the leaf batch sets it to 84 at `0x650515`. These findings do not yet validate the full pixel alpha expression or justify replacing all production material handling with one constant.

The 59-row summary is `Findings/SpeedTree-wibo-geometry-lod-survey.tsv`; full traces remain local. Representative 101-sample fixtures for types 0, 12 and 39 are checked in under `tools/speedtree/fixtures/lod-geometry/`, including both discrete and two-leaf mode cases. Reproduction:

```
.cache/zig/ziglang/zig c++ -target x86-windows-gnu -O2 -static -municode tools/speedtree/lod-geometry-probe.cpp -o .cache/lod-geometry-probe.exe
mkdir -p .cache/geometry-traces
seq 0 58 | xargs -P 4 -I '{}' sh -c 'id=$(printf "%03d" "$1"); .cache/wibo-build/wibo .cache/lod-geometry-probe.exe "$PWD/google_drive/speedtreert.dll" "$PWD/.cache/tree-source/tree-$id.spt" 100 > .cache/geometry-traces/$id.tsv' sh '{}'
PATH="$PWD/.cache/rust/bin:$PATH" python3 tools/speedtree/compare_geometry_lod_rust.py .cache/geometry-traces
PATH="$PWD/.cache/rust/bin:$PATH" python3 tools/speedtree/compare_geometry_lod_rust.py tools/speedtree/fixtures/lod-geometry --types 0,12,39
```

Neither the per-component selection nor the alpha references are yet used in production. The cache still lacks all component LODs, real bounds and full transition state; current shader state and leaf animation remain approximate. P4.01–P4.03 are still **No**.

`src/tree_lod.rs::client_components` now combines the checked discrete and transition translations into branch, frond and up-to-two leaf slots with individual cutout references for drop-to-billboard disabled and leaf modes 0/1. Unsupported modes and invalid inputs return `None`; it does not invent geometry or implement billboard transitions. The 5,959-trace comparison now exercises this combined function. It remains unwired until the cache supplies independent component geometry and metadata; it does not change the switches.

### Experimental PWTREE08 leaf normals

The client leaf draw at `0x65714b–0x6571c5` reads one normal per leaf card
from `Leaf+0x28` (three floats per card) for all four vertices. The unshipped
baker source now writes these three floats after each leaf center under magic
`PWTREE08`, and rejects missing or nonfinite leaf normals with a tree type and
leaf-material diagnostic. The reader accepts 05–07 unchanged, with no recorded
leaf normal, and accepts 08 with the extra normal. The far-tree vertex shader
can consume an 08 normal (with the same Y/Z coordinate swap as indexed geometry);
legacy caches retain the previous up-normal fallback. This is **not** a material
or lighting recovery: fragment lighting is still an estimate, and normals have
not been proven invariant under all runtime wind/timing states.

The embedded helper and setup cache key remain PWTREE07, so ordinary production
setup does not currently generate 08 caches. The source baker still stores three
joint snapshots, not independent component levels; this does not complete P4.02.
Do not replace the embedded helper based on the compatibility run below.

Validation on the current sandbox:

* `.cache/zig/ziglang/zig c++ -target x86-windows-gnu -O2 -static -municode tools/speedtree/bake.cpp -o .cache/speedtree-bake-08.exe` succeeded; `g++ -std=c++17 -O2 tools/speedtree/test_bake_geometry.cpp -o .cache/test-bake-geometry && .cache/test-bake-geometry` passed.
* Running one SPT at a time with `.cache/wibo-build/wibo .cache/speedtree-bake-08.exe google_drive/speedtreert.dll <single-file-directory>` produced 56 caches; 34, 47 and 48 still abort during strict branch-normal checks because Wibo cannot unwind the exception. The helper exits 1 for the single-file runs because the other 58 input files are absent.
* An isolated `rustc --test` extraction of the actual reader and its tests, with a stub GBK decoder, parsed all 56 outputs (11,263 leaf records with finite normals), and passed the 05/06/07/08, truncation and nonfinite-normal tests. This does not test real GBK decoding, full Cargo compilation, or GPU shader/pipeline validation. Full Cargo tests remain blocked by unavailable crates.io dependencies. The first normal for type 0 was `[0.7135655, 0.2773056, 0.6433707]`.

### Independent component-index retrieval and leaf-normal wind samples

`GetGeometry` at `0x10024680` passes its three short override arguments to
branch `0x100247d0`, frond `0x10024a90`, and leaf `0x10024db0`, respectively.
`tools/speedtree/lod-explicit-probe.cpp` compares automatic geometry at each
component LOD's midpoint with explicitly requesting that same index at the
opposite continuous-LOD endpoint. For indexed geometry it hashes count, strip
lengths and indices, positions, normals and UV0; for leaves it hashes count,
centers, normals, cluster indices, UVs and camera-facing map coordinates. It
also compares every combination of explicit branch/frond/leaf indices with
individually requested components, at continuous level 0.37. Matching hashes
are evidence about those fields, not an assertion of byte-by-byte proof, nor
about unexamined colors, tangents, material state, other cameras, or wind.

Under the same wibo/Wine-derived CRT setup described above, all 59 records
completed: **369/369** individual component levels and **721/721** explicit
index combinations matched. The completion-checked per-type results are
`Findings/SpeedTree-wibo-explicit-lod-survey.tsv`; three example traces are in
`tools/speedtree/fixtures/lod-explicit/`. This establishes a practical diagnostic
path for extracting independent levels for these assets, but does not implement
or ship an independent-level cache or renderer. In particular, setting an
explicit index is not a substitute for obtaining the correct client transition
alpha and billboard state at runtime.

`tools/speedtree/leaf-normal-wind-probe.cpp` separately reads each explicit leaf
LOD's normals at rest, then enables rocking and makes 64 sequential calls at
0.25-second intervals at strength 1. In 59 completion-checked types, 121 leaf
LODs and 8,530 leaves, zero of 1,637,760 sampled normal components changed
from rest (`Findings/SpeedTree-wibo-leaf-normal-wind-survey.tsv`). This is
limited to that camera, constant strength, times and call sequence, under the
compatibility runtime; it does not establish general animation invariance or
client lighting behavior.

Commands:

```sh
.cache/zig/ziglang/zig c++ -target x86-windows-gnu -O2 -static -municode tools/speedtree/lod-explicit-probe.cpp -o .cache/lod-explicit-probe.exe
python3 tools/speedtree/survey_explicit_lods.py .cache/wibo-build/wibo .cache/lod-explicit-probe.exe google_drive/speedtreert.dll .cache/tree-source Findings/SpeedTree-wibo-lod-counts.tsv Findings/SpeedTree-wibo-explicit-lod-survey.tsv
.cache/zig/ziglang/zig c++ -target x86-windows-gnu -O2 -static -municode tools/speedtree/leaf-normal-wind-probe.cpp -o .cache/leaf-normal-wind-probe.exe
python3 tools/speedtree/survey_leaf_normals.py .cache/wibo-build/wibo .cache/leaf-normal-wind-probe.exe google_drive/speedtreert.dll .cache/tree-source Findings/SpeedTree-wibo-lod-counts.tsv Findings/SpeedTree-wibo-leaf-normal-wind-survey.tsv
PYTHONPATH=.cache/py python3 -m unittest discover -s tools/speedtree -p 'test_*.py'
```

Both cross-compiles succeeded; both surveys completed with the counts above;
18 Python tests passed. Full Cargo/GPU and native Windows validation remain
unavailable. P4.01–P4.03 remain incomplete.

### Experimental independent-component archive (PWCOMP01)

`tools/speedtree/component-cache-probe.cpp` now writes a **diagnostic**, not
production-ready, independent-component archive. After the runtime `Compute`,
it requests each branch, frond, and leaf index independently using the
GetGeometry short overrides. It writes the DLL and SPT FNV-1a identities and
tree type, raw branch and composite texture names, then each component's
separate ordered LOD records. Branch/frond records reuse the strict checked
indexed-geometry serializer (positions, normals, UVs and triangle-list indices).
Leaf records store centers, normals, clusters, UVs and the DLL's four 4-float
card entries for the fixed +X camera at rest. The file deliberately has a
separate `PWCOMP01` magic: it must not be mistaken for a PWTREE07/08 file.

`src/tree_component_cache.rs` is a standalone, bounded reader with identity,
count, finite-value, index, truncation, and trailing-byte checks. It is not
connected to viewer startup. This archive omits material state, alpha-reference
transition parameters, general-camera leaf transforms and wind evolution;
its fixed-camera cards cannot be rendered correctly for arbitrary cameras.
It is a building block for a real cache, not an implementation of P4.01–P4.03.
The shipped helper, startup key, and snapshot renderer remain unchanged.

With wibo/Wine-derived CRT, 56 of 59 type archives were emitted and parsed by
the Rust reader with component counts matching the runtime survey. The 115
leaf LODs in those 56 archives have fixed-camera field hashes matching the
independent automatic-LOD traces described above. This hash comparison covers
the listed leaf fields only; it does not independently verify indexed topology,
materials, other camera directions, wind, or native Windows execution. Types
34, 47 and 48 did not emit archives: as previously investigated, strict
nonfinite branch-normal errors cause wibo's missing `K32EnumProcessModules`
exception-unwind failure. The survey records these failures and exits nonzero;
no replacement normals are invented. Per-type counts and errors are in
`Findings/SpeedTree-wibo-component-cache-survey.tsv`.

Reproduction (the trace directory comes from the earlier explicit-LOD probe):

```sh
.cache/zig/ziglang/zig c++ -target x86-windows-gnu -O2 -static -municode tools/speedtree/lod-explicit-probe.cpp -o .cache/lod-explicit-probe.exe
mkdir -p .cache/lod-explicit-traces
seq 0 58 | xargs -P 4 -I '{}' sh -c 'id=$(printf "%03d" "$1"); .cache/wibo-build/wibo .cache/lod-explicit-probe.exe "$PWD/google_drive/speedtreert.dll" "$PWD/.cache/tree-source/tree-$id.spt" > .cache/lod-explicit-traces/$id.tsv' sh '{}'
.cache/zig/ziglang/zig c++ -target x86-windows-gnu -O2 -static -municode tools/speedtree/component-cache-probe.cpp -o .cache/component-cache-probe.exe
.cache/rust/bin/rustc --edition=2021 --test src/tree_component_cache.rs -o .cache/tree-component-cache-tests
.cache/tree-component-cache-tests
.cache/rust/bin/rustc --edition=2021 tools/speedtree/check-component-cache.rs -o .cache/check-component-cache
python3 tools/speedtree/survey_component_cache.py .cache/wibo-build/wibo .cache/component-cache-probe.exe .cache/check-component-cache google_drive/speedtreert.dll .cache/tree-source .cache/lod-explicit-traces Findings/SpeedTree-wibo-lod-counts.tsv .cache/component-survey Findings/SpeedTree-wibo-component-cache-survey.tsv
python3 tools/speedtree/compare_component_leaves.py .cache/component-survey .cache/lod-explicit-traces Findings/SpeedTree-wibo-lod-counts.tsv --missing 34 47 48
PYTHONPATH=.cache/py python3 -m unittest discover -s tools/speedtree -p 'test_*.py'
```

The survey intentionally exited 1 for the three failed types; the leaf
comparison checked 56 types and 115 levels; three standalone Rust tests and
20 Python tests passed. The Rust tests include all truncated prefixes of a
synthetic cache and invalid normal/index rejection. Full Cargo, GPU, and native
Windows tests remain unavailable.

### DLL wind-phase update and first leaf rotation angle

Further disassembly identified part of the runtime leaf-rocking path, but not
an end-to-end animation implementation. `SetTime` (`0x100210e0`) stores the
f32 global time at `0x100652e8`. `SetWindStrength` (`0x10021930`) passes the
new strength and, for the client's `-1, -1` arguments, the prior strength and
phase from the wind object into `0x1002bc00`. That routine stores the new
strength, computes rate `8 * strength * field[0x1c]` and amplitude in degrees
`180 * strength * field[0x18]`, and adjusts the f32 phase so the old and new
rates meet at the current global time. It also clears fields `+0x30` and
`+0x34`; their complete lifetime and alternative wind paths require further
investigation. Values at `+0x18` and `+0x1c` are required per-tree runtime
inputs, not yet extracted into a usable production cache.

The leaf-table caller at `0x10018543–0x10018583` selects either
`0x1002bd00` or `0x1002bee0` depending on field `+0x30`. For the former,
`0x1002bd0a–0x1002bd94` calculates one rotation angle: when rocking is active,
the extent is the derived degree amplitude and the phase is global time times
rate plus the stored phase plus a caller-supplied per-table phase. When it is
inactive, the extent is `field[0x18] * 0.15f` and only that caller-supplied
phase is used. The interpolation maps the sine from 0..1 into `[-extent,
extent]`. The caller forms that phase from table data and the group index;
it is **not** proven to be an instance-position phase. The remainder of
`0x1002bd00` builds a matrix, and the alternative `0x1002bee0`, table inputs,
other rotations, global camera state, and per-tree runtime state are not yet
translated. In particular, the new code cannot replace the sampled loop.

`src/tree_wind.rs::RuntimeWindPhase` contains an **experimental, unwired**
translation of the update and this first angle. An isolated Unicorn probe sets
x87 control word `0x027f`, resets the FPU between calls, verifies the intended
return/capture instruction was reached, and executes the actual DLL code with
synthetic memory. It does not run the client or Windows. Comparison results:

```sh
.cache/rust/bin/rustc --edition=2021 --test src/tree_wind.rs -o .cache/tree-wind-tests && .cache/tree-wind-tests
PATH="$PWD/.cache/rust/bin:$PATH" PYTHONPATH=.cache/py:tools/speedtree python3 tools/speedtree/compare_wind_phase_rust.py google_drive/speedtreert.dll
PATH="$PWD/.cache/rust/bin:$PATH" PYTHONPATH=.cache/py:tools/speedtree python3 tools/speedtree/compare_rock_angle_rust.py google_drive/speedtreert.dll
PYTHONPATH=.cache/py python3 -m unittest discover -s tools/speedtree -p 'test_*.py'
```

Two Rust tests passed; 5,403 synthetic state updates (including 20 sequences
of 120 updates with prior DLL state fed back) matched bitwise, and 4,000
scalar-angle cases matched within 0.0001 degree (maximum observed error
`5.0e-10` in printed decimal comparison). The Python suite passed all 20 tests
after installing the optional Speakeasy dependencies. These results assume the selected
FPU control word and sampled finite inputs; Rust f64/libm is not a guarantee of
x87 equivalence at all boundaries. Full Cargo/GPU, native Windows, and complete
leaf-motion validation remain outstanding. P4.03 remains **No**.

### Viewer integration: opt-in wind-signal preview

The viewer now offers **World → Client wind signal (preview)**, off by default
and persisted in settings. When enabled, `src/gpu.rs` samples the recovered
client scalar at the viewer's elapsed time and multiplies the existing tree
rocking slider by it before sending the blend to the leaf shader. Disabled
preserves the previous slider behavior; invalid or unsupported times fall back
to that legacy blend. A single cached `ClientWind` table is shared across
frames. This does not change branches, fronds, LOD selection or grass.

This is deliberately labelled an approximation: the shader still blends
against its existing 16-second sampled loop, its clock is not the fully traced
client manager clock, and the scalar cannot substitute for the DLL's live
leaf matrices and strength state. P4.03 remains **No**. The independent-LOD
archive and runtime phase/angle translations remain unwired because the
archive lacks general-camera leaf data, material and transition state, while
those translations alone do not produce complete leaf geometry. P4.01 and
P4.02 remain **No**. The shipped PWTREE07 helper is unchanged.

Validation: standalone Rust `view_settings.rs` tests passed 24/24 and
`tree_wind.rs` tests passed 3/3 (including opt-in/default/persistence and
bounded preview behavior). The prior Unicorn comparisons still pass 5,403
phase updates and 4,000 scalar angles, and the SpeedTree Python tests pass
20/20. Full Cargo compilation, GPU pipeline execution and native Windows tests
were not available, so this integration has not been exercised in the running
viewer in this environment.

### Merge with current main: cache format collision

Main introduced a distinct PWTREE08 format containing branch/frond alpha
references, while the unshipped experimental baker on this branch had used
PWTREE08 for leaf normals. Those layouts are incompatible. The merge keeps
main's PWTREE08 reader and startup/cache policy intact, and assigns the
experimental source baker's alpha-reference-plus-leaf-normal layout the new
magic PWTREE09. No PWTREE09 helper is shipped. Existing findings above about
the earlier experimental PWTREE08 describe that earlier format only; they do
not describe main's PWTREE08 or establish validation of the new PWTREE09.

The far-tree vertex shader retains main's authored-mip alpha references and
reads optional PWTREE09 normals from a separate vertex-indexed buffer, so the
same packed vertex field is not used for both purposes. Legacy caches retain
up-normal fallback. The experimental PWTREE09 source is now in
`tools/speedtree/bake_experimental.cpp`; diagnostics include that file. The
production `tools/speedtree/bake.cpp` remains main's PWTREE08 source because
main's Windows build rebuilds and embeds it; using the strict unvalidated
experimental helper there could prevent the viewer from starting for types 34,
47 and 48. The new combination and experimental helper have not been GPU-tested
or validated on Windows here. The viewer wind preview remains opt-in. The
merge does not complete P4.01–P4.03.
