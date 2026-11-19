# 11 — PW 1.4.2 height, grazing and streamed geometry

**S38 and S40 are Forsaken World 0.525.** Their v14/v15 scene headers and
height-only results are not valid for Perfect World retail RT-02b.

## Pinned PW inputs and the correction

All three [input sets](../../../../tools/fixtures/lighter-retail-inputs.txt)
have matching PW 1.4.2 scene headers (v12/v12) and verified TRN2/T2BK hashes.
Each has 256 decoded blocks, a 512×512 grid and 2-unit spacing. The audit
compares the viewer's far-mesh plan to stride-one triangles at 523,265
vertices and cell centers per map, and measures the orthographic silhouette
at 1°, 2° and 3° in both directions along each cardinal axis.

| Map / policy | Indices | Mean height error | Max height error | Samples >0.5 | Worst grazing error |
| --- | ---: | ---: | ---: | ---: | ---: |
| `a01`, old regular 4 | 98,304 | 2.1035 | 100.3922 | 29,807 | 25.0636 |
| `a01`, regular 2 control | 393,216 | 0.8198 | 100.3922 | 12,797 | 26.3773 |
| `a01`, current adaptive | 213,666 | 0.0000 | <0.0001 | 0 | <0.0001 |
| `a02`, current regular 4 | 98,304 | 0 | 0 | 0 | 0 |
| `a07`, current adaptive | 146,958 | 0.0041 | 0.49785 | 0 | 0 |

All errors are in world units. `a01` needs local refinement: an all-grid
stride of one would use 1,566,726 indices, exceeding the 600,000-index
small-map refinement ceiling. The production planner refines 1,069 tiles
for `a01`, 466 for `a07`, and keeps the flat `a02` regular. The adaptive
mesh also passes its triangle-overlay error bound; this is not a material
or screen-space image comparison. A separate 4×4-block cutoff audit
intentionally omits 240/256 source blocks. A missing T2BK is `load_failed`,
not cutoff. See `11-terrain/pw142-{refined,strides,cutoff}.json` in the
[evidence bundle](14-delivery.md).

## Streamed and rear-view geometry probes (S)

On `59458a5`, the actual Native mesh builder and Lighter packer were run
with fixture materials over a synthetic ridge and the three pinned T2BK
inputs. The view moves from XZ `(-146,120)` to `(121,-146)`, replacing the
near window `[3,4,8,9]` with `[7,8,12,13]`. Meshes are split and rebuilt at
those windows. Each uncut packet has **15 far patches admitted, 1 replaced**,
2,044 exterior edge units and no unmatched interior edges at 0.00025-unit
height tolerance. Packed identity placements carry blocker and secondary
masks. Two CPU vertical probes outside the front view hit the expected
near/far owners at source height (zero measured error at these points).

| Height input | Packed indices, first → second window | Rear height errors, near/far |
| --- | ---: | --- |
| Synthetic ridge | 274,380 → 271,230 | 0 / 0 |
| `a01` | 343,572 → 347,586 | 0 / 0 |
| `a02` | 240,864 → 240,864 | 0 / 0 |
| `a07` | 276,999 → 277,107 | 0 / 0 |

With cutoff, the second window keeps the near hit and intentionally drops
all 16 far patches, leaving 153,600 indices / 640 exterior edge units. The
rear far probe misses, with no load failure or interior crack. Reproduce by
setting `PW_TEST_RETAIL_T2BK` to each verified `*_1.t2bk` and running the
ignored `lighter_streamed_retail_height_geometry` test. The synthetic test
is `refined_far_terrain_joins_changed_native_windows_without_cracks`.

**P still open:** the streams above are rebuilt geometry packets, not a
live travel run, and the rear probes are not GPU reflection/shadow pixels.
Inspect matched camera/grazing captures through actual residency changes
on the [Windows RTX system](01-sdk.md) before closing the physical gate.

## Ordered follow-up: diagonal grazing on pinned inputs (S)

`--diagonal-grazing` adds both diagonal azimuths, forward and reverse,
at 1°, 2° and 3° to the CPU profile audit. For each integer diagonal slice
it intersects the actual triangle edges, including fractional crossings;
nearest vertex sampling could miss the silhouette peak. A nonplanar quad
fixture detects a diagonal change that the cardinal profiles miss.

On the same pinned `a01`, `a02` and `a07` with the production indices,
each map supplied **12,276 diagonal profiles**, with **0 beyond 0.5 units**
and **0 measured diagonal silhouette difference** at the sampled slices.
The grid results and input checksums match the prior audit. See
`11-terrain/pw142-diagonal.json` in the ordered bundle. These are CPU
orthographic height silhouettes, not the live perspective or GPU
secondary-view screenshots. Neither the prior stream/rear probes nor
this audit closes the physical RT-02b gate.
