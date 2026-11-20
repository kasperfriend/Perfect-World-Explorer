# Phase 3 Agent C test artifact (P3.06, P3.07, P3.09)

This is the acceptance artifact for the three designated Agent C points. It is
the compiled-viewer/CI test artifact, not a separate test harness: every command
below exercises code reachable from `src/main.rs`'s normal world path or the
same standalone modules that CI compiles. Acceptance basis supplied by the
user: "Fulfill the designated scope targets, if they're achieved - flip to yes
and send me to test artifact"; artifact scope: "All that is designated to you
only".

## What to test

1. Open the viewer from PR #12 (`https://github.com/kasperfriend/Perfect-World-Exlorer/pull/12`)
   with the shipped `configs/angelica.cfg` (`WaterEffect == 0`,
   `detail_scene = 1` → quality preset 2).
2. Move the camera across a region boundary. The World tab's **Scenery quality
   preset** slider changes all ten `CECSceneBlock` class radii; the log line
   reports `10 native classes`, the tree LOD scale, resident placements,
   ornaments retained, loads, evictions and distance culls. `Objects::load_ids`
   retires the ornament models/textures that left the resident set. The Graphics
   tab's **Resolution scale** slider is preserved from `origin/main` alongside
   the World-tab scenery preset.
3. Compare main, fog, reflection, refraction, minimap and full-map views. Hidden
   / reflect-only / refract-only EC-BSD placements are gated by `Visibility`;
   home ornaments and EC models are hidden in reflection/refraction at the
   shipped default. `RenderContext` carries `WaterEffect` / underwater state and
   `allows_with_context` / `allows_class_with_context` encode the recovered
   class gates; the unauthored `+0x46`/`+0x47` object-state flags remain a named
   gap, so non-default contexts fail closed rather than guessing. All six views
   reuse one `FrameSnapshot` (no second time or light publication).
4. Run the CI checks below. The Windows release build is the compiled viewer
   artifact; the Linux job runs the static-model GPU programs under software
   Vulkan.

## Commands and local results (2026-11-03)

Local checks used the npm `@rustbin` 1.88.0 `rustc` / `rust-std` / `rustfmt`
toolchain with the copied sysroot, plus a `tree_sitter` venv. Full-crate
`cargo fmt` / `cargo check` / `cargo test` and the GPU diagnostics are validated
by CI, not by this sandbox.

```
rustc --edition=2021 --test src/scenery_visibility.rs   → 10 passed; 0 failed
rustc --edition=2021 --test src/view_settings.rs        → 23 passed; 0 failed
sh tools/check-phase1-core.sh                            → 33 standalone module test binaries + 1 multi-module harness, all ok
python3 tools/check-eight-phase-status.py                → consistent true, 26 / 1 / 49, errors []
python3 tools/audit-static-model-gpu-programs.py         → OK: 7 wired, 17 excluded with reasons, 24 total
python3 tools/test_phase1_ready.py                      → 7 OK
python3 tools/test_eight_phase_status.py                → 5 OK
rustfmt --edition 2021 --config skip_children=true --check <14 changed Rust files> → exit 0
git diff --check                                        → exit 0
Tree-sitter parse of 14 changed Rust files               → errors []
```

## CI acceptance

The `pr-checks.yml` workflow passed for the code merge (`bd5c8e9`) and both
documentation updates (`09dace7`, `17b8061`). PR #12 is `mergeable: true` /
`mergeable_state: clean`; its head is
`17b8061d5eefbe69cfa5ad00655ff577a2955b93`.

- Code merge run: `41629090684` — Linux `125893771507` (success, 1m41s), Windows `125893771251` (success, 1m32s)
- First documentation run: `41629531457` — Linux `125895258174` (success, 1m18s), Windows `125895258547` (success, 2m5s)
- Final documentation run: `41629943149` — Linux `125896651543` (success, 1m32s), Windows `125896652229` (success, 3m43s)
- Conclusion: `success` (all three runs)
- `gh pr checks 12`: `Rust checks and tests (Linux) pass; Windows executable (x64) pass`

Workflow steps include `cargo fmt --all -- --check`,
`cargo check --locked --bins`, `cargo test --locked --bin gfx-light-audit`,
`cargo test --locked --bin perfect-world-explorer`, 19 GPU diagnostic runs (24 diagnostics
total, 7 wired static-model + 17 excluded with reasons) under software Vulkan, the eight-phase and static-model audits, the
standalone `rustc --test src/scenery_visibility.rs` run, and the Windows release
build. Relevant GPU evidence: `scenery-pass-gpu-check` enumerates 6 passes x 6
recovered BMD states (36 rows) plus hidden/reflect-only/refract-only raster
gating, a real eviction and context-gate assertions; `bmd-depth-gpu-check`
rasterizes two coincident triangles under `GREATER_EQUAL`; `bmd-baked-gpu-check`
has 2,048 bit-exact day/night cases.

## Status

`Findings/Phase-3-completion-checks.md` records the integrated review result:
P3.06, P3.07 and P3.09 are **Yes** for their designated scope. Other Phase 3
rows and the phase completion gate are untouched. Full live-client parity is not
claimed; remaining non-designated gaps are registered in
`Findings/Unsupported-client-paths.md`.
