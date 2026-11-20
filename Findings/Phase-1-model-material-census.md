# Phase 1 — actual skin/material receiver census

## Scope and results (2026-11-02)

The private client archive has **40,753 entries**, including **10,898 SKIs**:
7,247 version 8 and 3,651 version 9. Its serialized mesh counts are 13,820
skin meshes and 1,988 rigid meshes, with no morph, supple or muscle records.
This is a data-format census, **not a renderer-support claim**.

The production Rust `Skin::parse` reader was compiled into an isolated corpus
harness and run on all extracted SKIs. **10,896 parsed; two were rejected**
for NaN weights (`0xffc00000` in the three serialized weight components):

- `models/npcs/宠物/骑宠/大龙虾/大龙虾.ski`: vertex 738.
- `models/players/通用装备/飞剑/仙鹤/仙鹤.ski`: vertices 330, 638, 639.

These are bad authored weights, not evidence for removing the finite-value
guard. Rejected records remain failures. The successful records contain
13,158 materials, 13,816 skin meshes and 1,988 rigid meshes; all produced
finite material constants and zero point-diffuse alpha in the binding check.
The isolated harness used encoding_rs 0.8.35 from its upstream tag with the
installed rustc 1.88; this is not the locked application dependency build.
CI separately compiles/tests the actual application dependency graph.

The production `Scene::load` reader found **73 unique world placements**.
Following their normalized ECM → SMD → BON/SKI dependencies found **14 ECMs,
14 SMDs, 14 BONs and 14 SKIs**, with no missing dependency at these levels.
All 14 world SKIs passed the production Rust reader and material binding:
**41 skin meshes, zero rigid meshes and 23 materials**. None of the two bad
archive records above is among these world receivers. This check does not
validate all animation tracks, textures, hangers, or runtime eligibility.

The Python header walker independently found that every SKI's serialized
skin/rigid payload ends exactly at EOF. `Skin::parse` now also rejects
unrecognized trailing payload instead of silently ignoring it. A regression
covers this guard and another verifies distinct ambient/diffuse/emissive/
specular fields through the parser-to-material-binding bridge.

## Reproducible application diagnostics

```sh
cargo run --locked --bin skin-material-survey -- /path/to/element/models.pck
cargo run --locked --bin ecmodel-check -- /path/to/element
```

`skin-material-survey` parses all SKIs and checks material binding for point
on/off, reporting a failing status for any unresolved record (including the
two known bad files in this archive). `ecmodel-check` now checks the actual
world's referenced SKIs and material bindings, rather than only checking that
filenames exist. Missing dependencies and parse/binding failures produce a
nonzero exit status. Neither diagnostic draws the receivers.

## Renderer gate

**Still open:** the 73 placements are not yet drawn by a world skin renderer.
The verified parser and material constants do not replace that integration.
Baked BMD/terrain and the approved tree path remain untouched.

### World skeleton/descriptor follow-up

The production `parse_ecm`, `parse_smd` and `Skeleton::parse` readers were also
compiled into the isolated harness (glam 0.27.0, encoding_rs 0.8.35). All 14
world descriptor/skeleton chains passed: nine SMD8/BON6 chains and five
SMD5/BON5 chains. The latter contain 77 embedded bone tracks in total; the
14 BONs contain 154 bones in total. No world SMD in this census has hangers.
Subsequently resolved all 18 external STCK1 files for the nine BON6 chains:
188 joint tracks, valid owning-skeleton joint IDs, complete payload consumption,
and finite position/quaternion samples at start/mid/end (564 transform sample
times). `ecmodel-check` includes these checks and missing tracks in its
nonzero-exit gate. Native animation/descriptor tests pass; full application
build pending CI. Runtime pose/palette conventions and world drawing remain
unverified, not replaced by this data census.

### Ordinary palette evaluation follow-up

The ordinary unscaled pose path now evaluates the actual world's 14 chains
at 69 start/mid/end poses and deforms 26,643 vertices without invalid palette
references/nonfinite results. This checks data and deformation viability,
not image parity, ECM action scheduling or a world draw. Joint IDs are mapped
to child bones and v9 skin names are mapped to skeleton palette indices;
unsupported reflection/scale/controller paths explicitly fail.

Important distinction: the zero **SMD** hanger count does not imply zero
**ECM** attachments. The descriptors contain 36 EventType-100 GFX references.
Combined ECM actions are not always the corresponding SMD action names.
These are still required by the renderer/effect integration, not silently
covered by the earlier skeleton/material census.

### Authored texture payload follow-up

Source-backed lookup tries `Tex_<skin-stem>` then `Textures` beneath the
**SKI** directory, including a generic retry if preferred decoding fails.
All 41 texture references resolve: 16 skin-specific and 25 generic, representing
39 distinct DDS files. The production bounded DDS reader retains/validates
383 referenced authored mip levels without regenerating them. All base images
also decode independently with Pillow: 29 opaque and 10 graded-alpha images.
This does not close material sampler, alpha, shader/effect or world draw
integration. Unsupported DDS format/shape paths remain explicit errors.
