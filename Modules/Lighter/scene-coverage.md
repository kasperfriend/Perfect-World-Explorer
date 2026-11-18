# Lighter scene coverage (RT-03 and RT-04)

## Capture an installed map

Use one matching, supported Perfect World client tree. Keep the map files, packages and optional SpeedTree cache from that same build. Choose a camera in world units; yaw and pitch are radians.

```sh
mkdir -p target/lighter-audit
PW_PREVIEW_LIGHTER=software PW_PREVIEW_COVERAGE=1 PW_PREVIEW_WARMUP=12 \
  ./target/release/perfect-world-explorer /path/to/client --world a01 \
  --gpu-preview-at 0 40 0 0 -0.35 target/lighter-audit/a01.png
```

The `*.coverage.csv` sidecar reports the installed ray snapshot, not everything visible at the camera nor everything shipped by the client. The `*.coverage.txt` sidecar identifies the map, camera, settings, GPU/driver, and has a SHA-256 of the **decoded RGBA8** image. Use a fresh output directory for each run. `PW_PREVIEW_WARMUP` is optional (0–120 frames, fixed 16 ms logic steps); without it the headless capture renders the first frame. The capture pins the time of day and turns off history blending so earlier frame content cannot fill in absent geometry. Roots may still be queued or out of their activation window. Headless does not start Walker/Populator. The viewer's Lighter panel shows the static and moving geometry subtotals during play.

A required load/build failure ends the command with an error; it does **not** publish a row of zeros or retry on the main world. A missing optional archive doesn't prevent unrelated classes from appearing. Hardware mode needs a real, supported Vulkan adapter and does not fall back. Pixel comparisons still need identical camera/extent/settings and a matched client build.

### What the rows count

| Class | Unit and boundary |
| --- | --- |
| `near_terrain` | Selected window blocks. Other blocks are represented by the far-patch row, not counted as missing near blocks. |
| `far_terrain` | Candidate patches; fully replaced patches and cutoff omissions are separate. A partially stitched patch with triangles counts once. |
| `trees`, `grass` | Tree placements and the instances actually supplied by the native grass streamer. Grass outside that stream is not enumerated; missing/unsupported grass data in the upstream loader is not inferred from an empty draw list. |
| `buildings` | Unique ornament placements. Includes the optional ray-only extension, package/member failures, both placement and packet budgets, and distinct range/cutoff exclusions. Texture fallback counts references on admitted surfaces, not distinct texture filenames. |
| `water` | Unique loaded water areas. Empty areas have no wet cells; a wet area outside the enabled cutoff is skipped. Shore overlays and mesh-based falls are not included as water areas. |
| `world_models`, `walker`, `populator` | ECWLD actors, Walker model parts (body/face/hangers), and active Populator actors, respectively. A primary-only mask doesn't remove an actor. Add-on assets which were never activated are outside these counts. The SKI header census covers previously seen files per source (including failures), not a scan of every archive member. Critter references not mapped by this world-model feed are reported separately. |
| `effect_roots` | Current scene/model event owners: playing, still queued, inactive, rejected, unreadable, unsupported, or no readable package. Light-only players can be loaded with zero effect draws. Events which could not be enumerated from an unread model cannot be counted. |
| `effect_draws` | Emitted world/Walker quads, posed skin draws and explicit Walker decal cards admitted to the ray packet. A loaded root need not emit any draws in a particular frame. |

`loaded + skipped + failed` accounts for the units of a row. Skip reasons are range, cutoff, replacement, budget, pending, disabled/inactive and empty. Failure reasons are unreadable package, unavailable member/dependency, invalid data and unsupported data. Fallback references are **additional annotations**, not another group of objects; don't sum them into geometry totals. Missing authored mips are a different problem (RT-05).

## Verification scope

- **Step 8:** [CI on `21813ca`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44400799795) passed Linux and the Windows portable build/audit. Both Software and CPU Vulkan fixtures hit the animated off-camera Populator caster's shadow before movement and saw the receiver lit after movement, at unchanged receiver depth. Both found the rotated/scaled hook child in a reflection after it was hidden from the primary ray; moving the hook removed the reflected hit without rebuilding its BLAS. The primary mask also changes Vulkan instance opacity, so the fixed-function opaque commit cannot bypass it. The mask and hook changes do not recreate the child's BLAS. This is not physical GPU validation or a retail actor census.
- **Step 9 fixture:** `tools/fixtures/lighter-coverage.csv` is the expected inventory for `lighter_coverage_fixture_capture`. It renders a real 960×720 offscreen PNG through the main streamed Software path, verifies the counts against the saved CSV and binds them to the capture's pixel hash. [Linux CI on `c19e10e`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44403096687) passed the comparison and keeps the synthetic files in its `lighter-coverage-fixture` artifact. The streamed BMD regression separately checks that an optional asset denied by the GPU packet budget increments `budget` without packing part of its geometry.
- **Incompatible inputs:** the S38/S40 heights in the terrain report came from *Forsaken World 0.525*. The actual scene headers on both maps are **ECWLD v14 / EC-BSD v15**; the current Explorer scene and water loaders accept **v12 / v12**. Those height checks cannot serve as retail world captures. No full map capture or unsupported-SKI total is claimed for S38/S40.

For compatible inputs, a separate read-only ECWLD/EC-BSD catalog check walked
and checked the block boundaries and validated ornament export offsets and BMD
path suffixes for `a01` and `a02` (both v12). See
[header checks, counts and file hashes](scene-catalog-2026-11-18.json).
`a01` authors 328 unique ornaments and 304 placed effect roots; `a02` has 32 and
42. Neither has water, trees, grass, EC models or critters in these scene files.
These numbers are **authored**, not admitted or rendered. The two instances
are useful BMD/GFX capture targets but are not a substitute for the map mix in
RT-17.

### Retail Software captures — 2026-11-18

The [Lighter retail coverage run on `7a71750`](https://github.com/kasperfriend/Perfect-World-Explorer/actions/runs/44406210481)
loaded the two v12 maps with matching, SHA-256-pinned packages and saved both
960×720 PNGs, inventories and camera/driver metadata in `lighter-retail-coverage`.
The optional workflow downloads its inputs only on the runner; none of the
client packages are in the repository or the artifact. It verifies each map ID,
nonempty near terrain and full accounting of the authored BMD and GFX owners.

| Map | Near blocks loaded | Buildings loaded / skipped / failed | GFX roots loaded / pending / inactive / failed | Effect draws loaded |
| --- | ---: | ---: | ---: | ---: |
| `a01` | 225 | 310 / 18 / 0 | 25 / 195 / 84 / 0 | 141 |
| `a02` | 225 | 32 / 0 / 0 | 25 / 14 / 3 / 0 | 91 |

The 18 skipped `a01` buildings hit the packet/residency budget, not a parser
error. No admitted BMD material reference fell back to white in either capture.
The two runs use Software RT on lavapipe, a center camera at terrain height +160,
noon, and 24 fixed 16 ms warm-up frames. They do not measure a physical GPU or
cover retail water, vegetation or animated actors. Large pending counts reflect
the native root queue at the selected tick, not missing root definitions.

Decoded RGBA8 SHA-256:

- `a01`: `73403e084cdd9adfbac7979fba445441cd31688e65d718fc4afec619bf1a1e90`
- `a02`: `3a27f4d6e60a18a3fbb2380973f0aec785a25029b2429bc555001231ba18559e`

Coverage does not establish geometric accuracy. The independent height-only
audit found `a02` flat but `a01` exceeded the 0.5-unit tolerance at both strides
4 and 2 (peak **100.3922** in both; **29,807** and **12,797** of 523,265 samples
over tolerance). Stride 1 on those 512² fields needs 1,566,726 indices, above
the current 600,000-index regular-grid refinement ceiling. This is additional
RT-02b evidence, not a streamed silhouette comparison.
