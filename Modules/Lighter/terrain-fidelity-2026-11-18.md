# Far terrain heightfield audit — 2026-11-18

**Historical stress case, invalid for Perfect World retail RT-02b.** See the
[verified PW 1.4.2 a01/a02 audit](evidence/2026-11-18/11-terrain.md) for the
correct inputs and the remaining live-view gate.

The checksum-pinned `pw142/element` a01/a02 scene pairs parse as v12/v12.
Both select stride four. Against the source grid, a01 has 2.1035 mean and
100.3922 maximum height error (29,807 samples above 0.5 units); its worst
1–3° grazing error is 25.0636. Flat a02 has zero height/grazing error.
Those are offline measurements, not streamed or outside-frustum results.

S38 and S40 are real Forsaken World 0.525 heightfields (one T2BK v6 tile each,
64 blocks, a 256×256 grid, spacing 2 world units). This is **heightfield
geometry evidence**, not a matched-camera Native/Software/Hardware capture.
The available copies supply heights; the shell did not have a complete
matching client/map install. The matching `surfaces.pck`, `building.pck`,
`litmodels.pck` and `textures.pck` all exceed the Drive connector's 100 MiB
file limit. No materials, cutout masks, rain, asset streaming of these maps in
the viewer, or physical GPU performance were verified by this audit.

## Baseline and correction

The native reference uses stride-one heights and the viewer's triangle
split. For each candidate, the independent audit samples every native
vertex and native cell center (130,561 positions per map). It compares the
orthographic top silhouette at 1°, 2° and 3° above the horizon in four
cardinal directions (256 cross-sections per direction per angle). Both error
measures use world units and a 0.5-unit tolerance.

| Map | Stride | Mean abs. height error | Max abs. height error | Height samples over 0.5 | Worst grazing silhouette error |
| --- | ---: | ---: | ---: | ---: | ---: |
| S38 | 4 (old small-map default) | 0.0029 | 1.8052 | 158 | 1.4763 |
| S38 | 16 (large-map fallback comparison) | 0.0092 | 2.0183 | 358 | 1.9838 |
| S38 | 2 | 0.0010 | 1.2521 | 60 | 0.5868 |
| S38 | 1 (selected) | 0 | 0 | 0 | 0 |
| S40 | 4 (old small-map default) | 1.0133 | 144.3807 | 27,286 | 9.1210 |
| S40 | 16 (large-map fallback comparison) | 5.5641 | 153.6778 | 85,065 | 13.0415 |
| S40 | 2 | 0.3392 | 140.0888 | 7,161 | 3.7440 |
| S40 | 1 (selected) | 0 | 0 | 0 | 0 |

On S40, stride four interpolated y=274.61 where the source has y=130.23
(sample column 237, row 223). Its small mean relative to that peak doesn't
make the approximation safe for nearby shadow and reflection rays.

The ray feed now tests strides 4, 2, and 1, in that order, against the
sample tolerance and with the existing 600,000-index small-map threshold.
Both measured grids select stride one (390,150 indices each); the standalone
Rust harness also ran the production selector on the independently decoded
heightfields. Smooth small maps can still use stride four. If refinement
would exceed the threshold, Lighter retains the best tested affordable grid
and warns when it fails. Large maps that could not afford stride four keep
the previous stride-16 fallback and report its fidelity as unverified. The
threshold gates refinement, not the total memory of arbitrary world sizes.
The native raster renderer's LOD choices are unchanged.

A refined far grid can be finer than the installed near LOD. Far boundary
vertices are snapped to the native perimeter segment in that case. The
reverse case still inserts actual native perimeter points into coarse far
triangles. [Linux CI run 44399006844](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44399006844/job/135136594065)
passed the production Software and CPU Vulkan pixel/depth seam fixtures in
both directions and the synthetic streamed viewer. The real-map
numbers above exclude stitching and do not establish live seam parity.

## Reproduce

Run from the repository root with the two TRN2 files beside their matching
T2BK files. The default command checks the selected policy; explicitly
asking for the baseline is expected to return status 1 (geometry error).

```sh
python3 tools/test_lighter_terrain_audit.py
python3 tools/lighter-terrain-audit.py google_drive/{s38,s40}.trn2 --json /tmp/far-audit.json
python3 tools/lighter-terrain-audit.py google_drive/{s38,s40}.trn2 --stride 4 --stride 16 --stride 2 --stride 1
python3 tools/lighter-terrain-audit.py google_drive/{s38,s40}.trn2 --cutoff-window 2 2 6 6
```

An explicit 4×4 near window leaves 48 of 64 decoded blocks intentionally
omitted on each map. A TRN2 without its companion T2BK reports `load_failed`
(status 2), even when cutoff is requested. The [saved JSON](terrain-fidelity-2026-11-18.json) records SHA-256 hashes of the
input files, the selected stride, and every tested direction/angle's metrics. No client assets
are in Git.

Still needed for full viewer verification: the complete matching client
resources, matched-camera captures including streamed near/far joins and
off-screen receivers, and a physical Vulkan ray-query GPU for measurements.
