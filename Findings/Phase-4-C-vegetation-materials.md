# Phase 4-C — vegetation materials, authored mips, and CI

**Status: Implemented by user-approved scoped closeout (3 November 2026), not a claim of exact native parity.** Native sampler settings, branch/frond alpha compare, deeper mip tails, and tree lighting/reflection differences remain deferred and are listed below.

Updated: 3 November 2026.

## Scope and preservation decision

This change is limited to vegetation texture materials and regression wiring. It does not change tree geometry, leaf animation, grass placement, or grass-wind internals. The existing tree render path remains available and is still the default.

`ViewSettings::vegetation_authored_mips` defaults to `false`. Its UI tooltip says restart is required. The startup value is captured once and passed through the initial and background `build` calls and `Renderer::new`; changing the setting in a running viewer cannot switch the current renderer or a later background rebuild. The regional-tree experiment remains an independent setting. When the authored-mip switch is off, tree atlas generation, the shared world sampler, grass coverage-preserving mip generation, cutouts, and reflection draw submission retain their previous behavior. The original `image` decode still supplies level 0; the new DDS decoder only reads lower levels.

The opt-in path uploads authored lower mip levels where the atlas can isolate them. The tree bake/cache source now carries per-LOD SpeedTree branch/frond alpha references (PWTREE08), and the inspected client explicitly sets leaf ALPHAREF to 84 before drawing. These cutout changes are gated behind the opt-in setting. Native sampler tuples remain unresolved. Build caveat: the repository-embedded `tools/speedtree/speedtree-bake.exe` has not been rebuilt in this environment and still emits PWTREE07 locally; Windows PR and release workflows now rebuild the PE32 helper from updated source before compiling the viewer. Native branch/frond references will reach packaged builds after that workflow runs, but this new route remains unverified until CI and real-client cache generation pass.

## Sampler / mip / cutout policy

| Material | Native sampler evidence | Current viewer sampler (kept in experiment) | Mip handling | Alpha/cutout |
|---|---|---|---|---|
| Bark | Per-material MAG/MIN/MIP, address, and anisotropy tuple **not recovered**. Generic material setup is descriptor-driven (`0x796966`, `0x796e22`); no vegetation-specific anisotropy writer was found. | Linear mag/min/mip; clamp address; anisotropy 16. | Decode authored DDS levels 1–7 and upload without filtering when dimensions exactly match the rectangular tree-atlas slot. Unsupported resized/mismatched chains use the old generated path. | Runtime threshold comes from SGeometry `+0x38`; updated bake source writes the value per LOD in PWTREE08 and the opt-in shader consumes it. The checked-in helper binary remains PWTREE07; Windows CI rebuilds it from source, but runtime cache generation is still unverified. |
| Branch | Same explicit unknown sampler tuple as bark. | Linear mag/min/mip; clamp; anisotropy 16. | Same as bark. | Runtime threshold from SGeometry `+0x38` (`0x656bf5..0x656c01`); source/cache/shader path added per LOD and Windows CI rebuilds the helper; runtime cache generation and the native draw result are not verified. |
| Frond | Same explicit unknown sampler tuple as bark. | Linear mag/min/mip; clamp; anisotropy 16. | Same as bark. | Runtime threshold from SGeometry `+0x74` (`0x656d3e..0x656d4a`); source/cache/shader path added per LOD and Windows CI rebuilds the helper; runtime cache generation and the native draw result are not verified. |
| Leaf/composite | Same explicit unknown sampler tuple as bark. | Linear mag/min/mip; clamp; anisotropy 16. | Same as bark. | Leaf ALPHAREF 84 is explicitly set by the inspected client tree renderer before leaf drawing (`0x650513`); the opt-in tree shader uses the strict GREATER boundary at 84/255. This is static evidence for that draw path, not a full visual parity test. |
| Grass | MAG/MIN/MIP and anisotropy descriptor unknown. Native addressing evidence: stage 0 U/V WRAP, stages 1/2 CLAMP (`0x65127a..0x65129c`). Current world atlas uses the shared clamped 16x sampler, so the native-vs-viewer address/sampler combination remains explicitly unsupported. | Shared world sampler: linear mag/min/mip; clamp; anisotropy 16. | Opt-in path uploads authored levels 1–7 for square or rectangular native-size content, including non-power-of-two dimensions when the DDS chain matches the expected floor-halved sizes. It reserves a padded rectangle, preserves level 0 without resampling, and uses independent X/Y UV extents. Truncated/malformed, atlas-reduced, or deeper-tail chains still fall back or warn. | **GREATER, alpha reference 84** (`0x652129`, `0x652361..0x652371`), confirmed exact. The shader's alpha test remains exactly 84/255; a separate grass vertex field carries the Y extent so the alpha-reference value remains unchanged. |

**Atlas limit:** tree and grass atlas textures expose LODs 0–7. Levels deeper than LOD 7 are retained in the decoded cache but cannot be isolated per packed tile because the minimum 128-pixel tile reaches one texel at LOD 7. The current atlas cannot preserve those tail levels without cross-tile collisions. Any such tail is logged, not silently described as uploaded.

**Authored-chain basis:** the `trees.pck` census found 127/127 vegetation DDS files with complete authored chains (all DXT1/DXT3); `grasses.pck` found 105/105 DDS chains (DXT3). `textures.pck` was over the Drive connector's single-file transfer limit; that archive is not used for tree/grass texture loading here.

## Preservation proof and diagnostics

- Default setting is false and has a round-trip/default test in `src/view_settings.rs`.
- The default tree path continues to generate the atlas mips and use the existing 16x sampler. The default grass path keeps its square slots and the same alpha-coverage generator; the authored-mip opt-in may use exact rectangular/native-size slots. Its shader keeps the exact GREATER/84 test (84 fails, 85 passes); a separate grass vertex field carries the Y extent without repurposing the alpha-reference value.
- The regional tree-lighting switch and reflection submission are untouched by this change.
- `tree-lod-gpu-check` remains the baseline synthetic regression for LOD transitions, wind/time variation, regional RGB, and default-lighting preservation.
- `grass-instance-gpu-check` remains wired for the integrated grass instance path (including placement/wind behavior).
- `grass-gpu-mip-check` reads back and compares exact square and rectangular/non-power-of-two authored LOD-7 payloads in addition to its independent-tile/coverage checks.
- `tree-authored-mip-gpu-check` exercises the opt-in `FarTrees::new(..., authored_mips=true)` upload path with synthetic authored chains. The asset-dependent `tree-gpu-mip-check` remains excluded with a named reason because CI has no user `trees.pck` and baked PWTREE cache.
- `vegetation_material.rs` tests the GREATER/84 boundary, default sampler/cutout values, generated-vs-authored distinction, runtime tree-cutout rejection, and explicit sampler gaps. The module is included in both dependency-free core-test loops.

## CI and local verification

| Check | Command | Result / run identity |
|---|---|---|
| Phase status audits | `python3 tools/test_phase1_ready.py` | Passed (7 tests) |
| Phase status audits | `python3 tools/test_eight_phase_status.py` | Passed (5 tests) |
| Phase status consistency | `python3 tools/check-eight-phase-status.py` | Passed; no errors |
| GPU diagnostic audit | `python3 tools/audit-static-model-gpu-programs.py` | Passed: 10 wired, 15 excluded with reasons, 25 total |
| Shell syntax / whitespace | `bash -n tools/check-phase1-core.sh`; `git diff --check` | Passed |
| Rust/GPU CI | PR #17, run `41656708248` — https://github.com/kasperfriend/Perfect-World-Exlorer/actions/runs/41656708248 | **Passed**. Linux job `125988836765`; Windows job `125988837508`. |

Successful CI commands/results:

- Linux job `125988836765`: `cargo fmt --all -- --check`; `cargo check --locked --bins`; `cargo test --locked --bin gfx-light-audit`; `cargo test --locked --bin perfect-world-explorer`; Python status/audit commands above; and all configured GPU checks. The Phase 4 GPU commands were `cargo run --locked --bin tree-lod-gpu-check`, `cargo run --locked --bin grass-gpu-mip-check`, and `cargo run --locked --bin tree-authored-mip-gpu-check`; all passed in the green run.
- Windows job `125988837508`: the nine existing standalone CPU tests plus `vegetation_material` passed; `cargo build --locked --release --target x86_64-pc-windows-msvc --bin perfect-world-explorer` and the `bmd-overlap-survey` release build passed; artifact staging/checksum passed.

The sandbox has no local `cargo`, `rustc`, or `rustfmt`; Rust compilation, Rust tests, and GPU checks were verified by the Linux/Windows CI run.

## Exact changed files

- `.github/workflows/pr-checks.yml`
- `Findings/Eight-phase-reassessment-2026-11-02.md`
- `Findings/Phase-4-C-vegetation-materials.md`
- `src/bin/grass-check.rs`
- `src/bin/grass-gpu-mip-check.rs`
- `src/bin/grass-texture-check.rs`
- `src/bin/tree-authored-mip-gpu-check.rs`
- `src/bin/tree-gpu-mip-check.rs`
- `src/bin/tree-lod-gpu-check.rs`
- `src/bin/tree-motion-check.rs`
- `src/bin/tree-texture-check.rs`
- `src/bin/windows-tree-setup-check.rs`
- `src/far_trees_gpu.rs`
- `src/gpu.rs`
- `src/grass.rs`
- `src/grass_instances.rs`
- `src/grass_mips.rs`
- `src/main.rs`
- `src/trees.rs`
- `src/vegetation_material.rs`
- `src/vegetation_textures.rs`
- `src/view_settings.rs`
- `tools/audit-static-model-gpu-programs.py`
- `tools/check-phase1-core.sh`
- `tools/static-model-gpu-programs.txt`

## Follow-up native binary inspection (3 November 2026)

The downloaded `elementclient.exe` hash matches the expected client build; the downloaded `speedtreert.dll` is PE32 i386. Static DLL inspection found these relevant facts:

- `SGeometry` constructor (`0x1001dc40`) writes IEEE-754 -1.0 (`0xbf800000`) to offsets `+0x38` and `+0x74`. GetBranchGeometry/GetFrondGeometry subsequently write those outputs, now serialized per LOD by the PWTREE08 source writer. The shipped embedded PE32 helper still needs rebuilding for this serialization to be active.
- `SetLeafTargetAlphaMask` (`0x100201b0`) writes one byte at `+0x44`; it does not set either float field. CSpeedTreeRT initializes this byte to 84; the client renderer also explicitly sets ALPHAREF to 84 at `0x650513` before leaf drawing.
- The client reads geometry `+0x38` and `+0x74` at `0x656bf5` and `0x656d3e`, converts the float, and passes the result through `0x77a380`, which maps to render state `ALPHAREF` (24). This confirms the value's use, not the populated runtime value or matching alpha function.
- Client helper `0x779f60` writes texture-stage states MAGFILTER (16) and MINFILTER (17) from the same supplied value, and MIPFILTER (18) from that value when config byte `global_config+7` is true, otherwise 1. Its constructor defaults that config byte to true. This is generic client state plumbing; no verified call chain ties this helper or a resulting MAG/MIN/MIP tuple to vegetation.
- The grass draw's known `0x651287–0x65129c` calls remain address-state evidence only: stage 0 WRAP; stages 1/2 CLAMP. No vegetation-specific filtering or anisotropy tuple was established.

The PWTREE08 parser tests, shader regression, all-target Linux build, Windows helper rebuild, and x64 viewer build passed in CI run `41719211642`. This validates format compilation and shader integration, not runtime values from the user’s SpeedTree DLL or a visual comparison. Native sampler parity, authored mip tails beyond LOD 7, grass sampler details, tree alpha-compare parity, and tree lighting/reflection parity remain open.

## Remaining gaps

1. Native material sampler tuples (MAG/MIN/MIP, per-stage addressing, anisotropy) are not retained in the vegetation code path; the viewer sampler is preserved rather than claimed as an exact match.
2. PWTREE08 now serializes the getter-provided branch/frond references per LOD and the opt-in shader consumes them; older cache versions retain legacy behavior. The checked-in helper is still the old PWTREE07 binary; Windows PR/release workflows now rebuild it from source. Neither the bake nor the new parser/shader path could be built or exercised here because the sandbox has no Cargo or MinGW toolchain. End-to-end threshold parity is therefore not yet established.
3. Authored tree/grass mip tails beyond atlas LOD 7 cannot be isolated per packed tile; deeper levels remain cached but are not uploaded. Grass chains whose native-size rectangle cannot fit the current atlas still take the logged generated fallback.
4. Tree regional lighting remains the existing opt-in approximation; native per-material lighting constants are not recovered. Reflection submission remains in the production reflection/refraction path, but it lacks a standalone vegetation reflection readback comparison against client output.


### PWTREE08 cache work (2026-11-03)

`tools/speedtree/bake.cpp` now writes PWTREE08 with `Geometry.alpha_branch` and `Geometry.alpha_frond` after each LOD geometry pair, and emits a compact `tree-alpha-refs.csv` in the prepared cache for native inspection. `src/trees.rs` parses and range-checks those fields; older versions retain `None`. The opt-in far-tree vertex path transports the per-LOD reference and applies strict GREATER comparison; leaf reference 84 is separately evidenced by the client renderer. `windows_setup.rs` uses a PWTREE08 cache key. CI run `41719211642` successfully rebuilt the helper from source, parsed/tested PWTREE08, exercised the production shader regression, and built the Windows viewer. Follow-up CI run `41722258155` also compiled the added CSV alpha-reference report into the helper and passed Linux/Windows checks. The checked-in helper artifact remains older for local builds unless rebuilt; CI did not run the new bridge against the proprietary runtime DLL, and no real-client/render comparison was available. The alpha compare for branch/frond and sampler/reflection parity remain unresolved, so the user-approved status is Implemented for the best-supported viewer path; these are explicitly deferred parity gaps, not resolved findings.
