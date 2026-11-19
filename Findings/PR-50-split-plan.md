# Splitting PR #50 into four

Item 85 asks for the 61-file pull request to be split into four before it is
merged. That number is stale: the branch is now **108 files across 110
commits**, so the split is bigger than the item assumed. This is the plan, with
every file assigned to exactly one cut and the assignment machine-checked.

The split is **not done**. See "Why this is a plan rather than four branches".

## The four cuts

| cut | subject | files |
| --- | --- | --- |
| **A** | Client version scanning, and what each client actually ships | 20 |
| **B** | Package containers: `pck`, `angelica_pck`, census and DDS survey | 8 |
| **C** | PART 3, items 72–84: survey a constant, then relax it | 23 |
| **D** | Offline harness, CI gates, and the phase annotations | 57 |

Cut D is large because it is the residue: the harness and the register work
touched nearly everything, and it must land **last** — the CI gate that runs the
other three cuts' tests lives there.

### A — client version scanning (20 files)

The `0x0002000x` package revisions, the six old-vs-new client failures, the
elements/tasks data-version pair, the PE version-resource walk, and the
`version-gates.py` matrix.

    .github/workflows/pr-checks.yml
    src/client_detect.rs
    src/speedtree_version.rs
    src/modules/designer/{catalog,assembly,customize,face}.rs
    src/modules/librarian/{database,elements,mod}.rs
    src/modules/mod.rs
    src/bin/client-check.rs
    src/bin/environment-survey.rs
    tools/version-gates.py
    tools/instance-aliases.py
    docs/cross-version-matrix.md
    docs/blockers.md
    Findings/Unsupported-client-paths.md
    Findings/Gfx-version-ceiling-2026-11-18.md
    README.md

### B — package containers (8 files)

A `.pck` is two containers, not one; the 1.5.2 `0x00020003` revision; split
packages read from the start of their own piece; the encrypted refusal; the
inventory and the 1,433-member DDS survey.

    src/pck.rs
    src/angelica_pck.rs
    src/bin/pck-inspect.rs
    tools/pck_lib.py
    tools/archive-census.py
    tools/test_archive_census.py
    tools/dds-survey.py
    tools/test_dds_survey.py

### C — PART 3, items 72–84 (23 files)

Each item is the same shape: a constant stood in for a range the format allows,
so the shipped data is surveyed and everything the survey does not find becomes
a named refusal. This is the only cut whose scope is a numbered list, and the
register file for it is `Findings/PART-3-survey-relax-2026-11-18.md`.

    src/gfx_light.rs                 src/grass.rs
    src/gfx_light_audit.rs           src/tree_source.rs
    src/model_dds.rs                 src/water.rs
    src/model_texture_gpu.rs         src/scene.rs
    src/terrain_lightmaps.rs         src/material.rs
    src/terrain_gpu.rs               src/terrain_block.rs
    src/bin/{grass-check,grass-texture-check,grass-placement-survey,
             water-check,water-record-survey}.rs
    tools/lightmap-survey.py         tools/test_lightmap_survey.py
    tools/ecbsd-survey.py            tools/test_ecbsd_survey.py
    Findings/PART-3-survey-relax-2026-11-18.md
    Findings/Archive-file-type-census.md

### D — harness, CI gates and register (57 files)

`offline_harness.sh` and its modes, the binary closure tools, the launcher suite
and its `static ENV` race, the precinct and draw-state gates, and the 37
annotated register rows. This cut adds the tests the other three rely on, so it
lands last and the others are rebased onto it or tested by it.

## Why this is a plan rather than four branches

The four cuts cannot be produced safely from here:

1. **Session constraint.** This session may only push to
   `arena/3074ae8a-perfect-world-explorer`. Creating three more branches and
   pushing them would break that rule, and a branch cut is exactly what the
   item asks for.
2. **The history does not separate.** The 110 commits interleave: `a8f8952`
   (cut A) touches `main.rs` and `launcher_ui.rs`, which are cut D; `8b75000`
   (cut D) is 130 lines of `offline_harness.sh` that cut C's survey modes run
   inside. So the cuts are file-partitions of the *final tree*, not disjoint
   slices of the commit list. Splitting by commit would either duplicate work or
   produce commits that do not build.
3. **Each cut must build on its own.** With 57 files in D and every cut's CI
   gate inside it, a naive `git checkout` of a file subset yields commits that
   fail `cargo check`. Doing this properly means, per cut: check out the base,
   apply that cut's file set, repair the resulting compile errors, run the
   offline gate, and only then open the PR. That is four sequential cycles of
   work that each need a green CI run to be trustworthy.

## What has been verified

The assignment is exhaustive and disjoint, checked against
`git diff --name-only 6a8f2eca..HEAD`:

    108 changed files, 108 assigned
    UNASSIGNED: []
    ASSIGNED BUT UNCHANGED: []
    A 20   B 8   C 23   D 57

So the split has no orphan files and no file claimed twice, which are the two
ways this kind of partition silently goes wrong. What is *not* verified is that
each cut compiles in isolation — that requires the four branches.

## Recommended order

B, then C, then A, then D. B is the smallest and its readers (the package
containers) are what C's surveys run *inside*, so landing it first keeps C from
carrying a copy. A is independent and can land any time after B. D last, because
it is where the CI that runs the other three's tests is defined.
