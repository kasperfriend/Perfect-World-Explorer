# 10 — a07 lightmap binding and recorded-water probe

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

**S:** the standalone validator accepted **32/32** a07 DDS tiles, 64 more
256² tiles on `a01`/`a02`, and all nine parser/selector-expansion tests passed.
On `31bfeef`, the actual a07 records were also bound to a **synthetic**
terrain/water fixture at hours **12 and 0**: Native used BC1 with 16 paired
areas, Software staged 16 out of 16 at up to 64px into its decoded ray bank.
Both wrote 960×720 PNGs with lightmaps enabled; the Software receipts show
240×180 rays and three synchronized draw/readbacks. This verifies binding,
not retail art or Native-vs-ray lightmap parity. Checksums, logs and PNGs:
`10-water/` in the [bundle](14-delivery.md); input files are not bundled.
The CPU retail manifest requires lightmaps **on** and 16 paired areas in
both routes. The earlier lightmaps-off captures are not interchangeable.
The [12-pair CPU run on `d6c5c37`](09-retail.md) passed this admission gate.
The pinned `a07.cld` is now fetched under its own name; its 32 bytes match
the empty header previously carried over from `a01` byte for byte.

| View | CPU evidence | Physical Native comparison |
| --- | --- | --- |
| Shoreline, day | Whole encoded RGB8 MAE **5.619**, max **103**; 16 lightmap pairs bound. | Not reviewed on target |
| Shoreline, night | Whole MAE **5.061**, max **88**; 16 lightmap pairs bound. | Not reviewed on target |
| Shoreline, rain | Whole MAE **5.654**, max **104**; authored rain loaded. | Not reviewed on target |
| Above-water and submerged | Both eye-side checks and paired bindings passed. Separate per-ray extinction fixture below. | No on-target transition sequence or pixel verdict |

These are screen-region RGB numbers, not a wet-surface mask comparison.
The case screenshots are in the [hosted artifact](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44538569462/artifacts/14538576299),
not locally inspected; the signed download returned EOF.

The pinned water and height records identify water area **381**, water Y
**330.8305**, 32 wet cells and 18 shore-wave groups. Keep `(16,356,84,0,-0.6)`
for the matched above-water day/night shoreline control. For a transition,
try XZ **(80, −144)**, where the source mesh is Y **326.96274** inside a wet
cell. At the same yaw/pitch, compare Y **332.66** and **329** (about 1.83
units either side of the surface). Confirm the live camera and wet mask
before recording. Capture depth and wet path length alongside fixed ROIs;
water color alone cannot measure extinction when reflection and lighting
also change. `eye_underwater_depth` indicates viewer admission, not the
length of a ray segment. See `10-water/a07-depth-scout.log` and
`12-inventory/authored-audit.log`.

**Recorded-water S probe:** `lighter_a07_authored_medium` uses the pinned
ECWLD/EC-BSD/`bsdata/1.dat` records and production wet-cell packer and
shader. Above-water eye Y332.66 selects no body; Y329 selects area 381.
A diagnostic white receiver at the measured source bed height isolates a
**2.03726-unit** fully wet vertical ray. The sampled environment supplies
range **10–100** and the same underwater hue at hours 12 and 0. Predicted
linear RGB transmission is `[0.9167073, 0.9224511, 0.9344360]`. Both hours
read `[235,237,241,255]` linear UNORM8 with extinction and `[255,255,255,255]`
with extinction disabled, at identical reverse-Z `0.48277333`. This is an
authored footprint/fog check with synthetic geometry, not a Native shoreline
comparison, surface refraction test or physical result. Run with
`PW_TEST_RETAIL_A07` set to the verified map directory and `cargo test --bin
perfect-world-explorer lighter_a07_authored_medium -- --ignored --nocapture`.
Raw evidence: `10-water/a07-authored-medium.log`.

The actual `E:\pw142` run is blocked by [host access](01-sdk.md).
The connected Drive supplied the small records, but the matching
`textures.pck` (117,047,648 B) and `building.pck` (326,185,591 B) exceed its
100 MiB download limit; no alternate/mixed packs were substituted. Older
pixels are in the [hosted CPU receipt](09-retail.md); transfer status in
[delivery](14-delivery.md). The existing [analytic transmission tests](../../media.md)
measure RGB attenuation on synthetic segments, not the a07 retail view.
