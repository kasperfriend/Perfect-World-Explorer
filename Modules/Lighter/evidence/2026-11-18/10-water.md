# 10 — a07 lightmaps fixed; shoreline and submerged comparisons blocked

**Verified inputs:** both actual `a07_1.t2lm[1]` records match the pinned
[retail input hashes](../../../../tools/fixtures/lighter-retail-inputs.txt).
Their version is **2**, with **16 × 128² single-mip DXT1/BC1 tiles per
record**, DDS flag `1`, 8,320 bytes per DDS tile. The old hard-coded 256²
constraint was the issue, not an unknown version. The bounded parser now
admits 128² and 256² only and preserves all table/range checks. Native reads
from the **selected map directory** like Lighter, not `maps/world` for an
instance map. For Native BC1's existing 256² slot, a 128² source duplicates
selectors 2×2 without refitting endpoints. This is not a claim of Native
half-texel parity. Sidecars distinguish authored day/night, day-only and
unbound Native atlas areas, separately from the enabled setting.

**S:** the standalone validator accepted **32/32** DDS tiles and all nine
parser and selector-expansion tests passed. Checksums and logs: `10-water/`
in [delivery](14-delivery.md); input files are not part of the bundle or Git.
The CPU retail manifest now requests lightmaps **on** for `a07` in each
condition; no file hashes were changed. Prior CPU matrix receipts (lightmaps
**off**) are not interchangeable with the new setup.

| View | Physical Native comparison | Verdict |
| --- | --- | --- |
| a07 shoreline, day | No retrieved PNG or matched recapture with lightmaps on | Not tested |
| a07 shoreline, night | No retrieved PNG or matched recapture with lightmaps on | Not tested |
| a07 submerged / transition | No fixed pose with paired RGB and wet distance | Not tested |

The actual `E:\pw142` run is blocked by [host access](01-sdk.md).
The connected Drive supplied the small records, but the matching
`textures.pck` (117,047,648 B) and `building.pck` (326,185,591 B) exceed its
100 MiB download limit; no alternate/mixed packs were substituted. Older
pixels are in the [hosted CPU receipt](09-retail.md); transfer status in
[delivery](14-delivery.md). The existing [analytic transmission tests](../../media.md)
measure RGB attenuation on synthetic segments, not the a07 retail view.
