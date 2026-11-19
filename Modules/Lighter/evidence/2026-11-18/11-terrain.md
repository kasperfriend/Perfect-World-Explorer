# 11 — Correct-input RT-02b (height-only measurements)

S38 and S40 are **Forsaken World 0.525** and **invalid for the Perfect World
retail gate**. Their height-only audit remains useful as a separate stress
case, not as evidence for an Explorer-compatible retail scene.

Fresh inputs for `a01` and `a02` were retrieved from the same `pw142/element`
Drive tree as the pinned workflow packages. All eight TRN2, T2BK, ECWLD
and EC-BSD hashes match the [input manifest](../../../../tools/fixtures/lighter-retail-inputs.txt).
The [production scene parser](12-inventory.md) accepted their ECWLD/EC-BSD
v12/v12 pairs and ornament offsets. The height audit decoded 256 blocks
for each 512×512 grid (2-unit spacing) and measured 523,265 vertices and
cell centers per stride comparison. The stride-one triangle split is the
reference, not a retail render. The grazing envelope is measured at 1°, 2°
and 3° in all four cardinal directions; all errors below are in world units.

| Map | Stride | Height mean | Height max | Height samples >0.5 | Worst grazing error |
| --- | ---: | ---: | ---: | ---: | ---: |
| `a01` | 4 (selected) | 2.1035 | 100.3922 | 29,807 | 25.0636 |
| `a01` | 2 (test) | 0.8198 | 100.3922 | 12,797 | 26.3773 |
| `a01` | 1 (test) | 0 | 0 | 0 | 0 |
| `a02` | 4 (selected), 2 or 1 | 0 | 0 | 0 | 0 |

**a01 fails the 0.5-unit height criterion.** The selector minimizes maximum
sample error among affordable options, so stride two's equal peak doesn't
replace stride four. Stride one needs 1,566,726 indices, above the current
600,000-index refinement budget. No terrain fidelity or memory policy was
relaxed to get a pass. `a02` is flat; it passes the **height-only** test.

A simulated 4×4 near window omitted **240/256 decoded blocks** per map by
design. This is `intentional_cutoff`, unlike the separately tested missing
T2BK `load_failed` case. Repeat with `tools/lighter-terrain-audit.py` and
`--stride 4 --stride 2 --stride 1`, or `--cutoff-window 2 2 6 6`. See
`11-terrain/` in the [bundle](14-delivery.md) for the full directional JSON,
input hashes and tests.

**Streamed and outside-frustum retail views: not tested.** The CPU streamed
and off-camera pose tests use synthetic scenes. Neither the offline grid nor
the old a01/a02 overhead captures verify seam or secondary-ray parity on
these inputs. The [Windows RTX client blocker](01-sdk.md) still applies.
