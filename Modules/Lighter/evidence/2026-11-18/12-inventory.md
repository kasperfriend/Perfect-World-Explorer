# 12 — Authored and run-time omissions are separate

Both `unsupported_version` entries in the [dated catalog](../../scene-catalog-2026-11-18.json)
are accounted for: `s38` and `s40` are Forsaken World 0.525, **ECWLD v14 /
EC-BSD v15**, not the **v12/v12** Perfect World records the viewer supports.
They cannot be admitted by changing the version bytes or using their heights
as a proxy for their scenes. The entries remain unsupported, not counted as
rendered or as broken PW 1.4.2 assets.

## Read-only census on pinned PW 1.4.2 inputs

`cargo run --locked --bin lighter-scene-audit -- ELEMENT/gfx.pck ELEMENT/maps/a01 ELEMENT/maps/a02 ELEMENT/maps/a07`
uses the production scene, GFX container and water readers. The first census
below covered **direct placed roots**. The follow-up traverses their authored
child references too. Neither binds textures, activates a player or visits
model-event owners. Checksums and raw census receipts are in `12-inventory/`.

| Map | Ornaments | Direct placed roots (unique paths) | Readable / unreadable / rejected / unsupported | Authored Warp elements (placement-weighted) | DstColor or InvDstColor source / destination elements | Grass area refs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `a01` | 328 | 304 (9) | 304 / 0 / 0 / 0 | 96 | 0 / 0 | 0 |
| `a02` | 32 | 42 (7) | 42 / 0 / 0 / 0 | 38 | 0 / 0 | 0 |
| `a07` | 116 | 22 (8) | 22 / 0 / 0 / 0 | 5 | 0 / 0 | 0 |

A readable file is **not** a playing root, and a zero direct-root blend
count says nothing about children or later model events. All three maps
have no grass types or grass refs; **this is not a mainland grass census**.
Lighter uses instances supplied by the Native grass stream. Beyond that
stream there is no authored blade-instance count. Camera range, residency,
unsupported/missing art and absent instance generation cannot be inferred
from an empty ray draw list. Ray preview receipts label the scope
`grass_inventory_scope=native_stream_only`.

## Runtime scope and known omissions

Historical [CPU captures on `7a71750`](../../scene-coverage.md#retail-software-captures--2026-11-18),
24 warm-up frames at noon, had `a01` **25 playing / 195 queued / 84
inactive / 0 failed**, and `a02` **25 / 14 / 3 / 0**. These are from those
frames only. New CPU receipts add a Native main-stream census and
case-level counters for each route and weather. See the [CPU
matrix](../../retail-matrix.md). There are no physical per-map counts.

New schema-3 receipts expose `world_effect_roots_*` for playing, queued,
inactive, rejected, unreadable, no package and unsupported in **both**
routes. These count actual world and model-event owners sampled at the
saved frame. Native `main_streamed_v1` reconciles authored BMD, water owners,
trees, EC actors and grass areas with main-stream draw decisions/reasons.
It excludes reflection/refraction subpass visibility. `native_last_scene_passes`
may be three even though the census counts scene owners once. In the
fixture, one BMD loads, one is out of range and one is unavailable, with one
texture fallback. Warp counts are the elements omitted by the shared player at
that tick, separately from roots. `Warp:1` requires a framebuffer distortion
pass which neither viewer route implements; it isn't glass or an alpha card.

`world_effect_dest_color_*_candidates` counts emitted world quads before
geometry/budget/depth filtering. D3D source factors 9/10 with destination
factors 1–6 can be composed in linear HDR, not with Native's encoded
attachment clamp. Non-affine pairs (including destination factors 9/10)
are omitted by the ray route; the `effect_draws.unsupported` total includes
other unsupported blend pairs too. These are **not** visibility or parity
counts. Compare matched PNGs if any candidates are present.

Both routes read `textures/rain.tga` with an explicit TGA decoder when
signature detection fails, instead of treating a valid TGA as missing.
`rain_mask_source` is `authored`, `package_missing`, `package_unreadable`,
`member_missing`, `member_unreadable`, or `decode_failed`;
`rain_mask_authored` is the compatibility flag. The unit fixture loads a
real encoded TGA and covers missing archive, missing member, invalid art
and unreadable directory. A historical `false` cannot identify which case
occurred. `rain_at_eye` includes the main-eye wet guard.
`native_rain_slots_selected` counts slots before shader wet-mask/clip/depth
checks; `ray_rain_cards` counts packed cards. Neither measures visible
pixels. Use controlled clear-vs-rain deltas and inspect the affected regions.

On the latest matched synthetic dry-eye fixture, toggling only Clear to
Rain changed **164 / 691,200** Native pixels and **3,034 / 691,200** Software
pixels in encoded RGB8. Both selected **320** submitted slots/cards and
reported `member_missing` from the same fixture package. This is a weather
A/B, not a counted rain-fragment mask or parity result. The Native frame
had three scene passes. See `12-inventory/final-matched-rain.json` and the
PNGs/hash-bound sidecars. These counts are not retail.

### Pinned client CPU captures on `d6c5c37`

Both routes reported **`rain_mask_source=authored`** in the hosted subset;
the fix did not merely change the fallback label. In the dry-eye views,
Rain selected 320 Native slots and 320 Software cards. Controlled deltas
are whole pre-UI pixels that changed from Clear, not surviving-fragment
counts or evidence that the two masks/appearances match pixel by pixel.

| Map (fixed daylight camera) | Native / Software changed RGB8 pixels (of 691,200) | Native / Software BMD loaded / skipped / failed | Playing / queued / inactive roots, each route | Warp omissions, each route |
| --- | ---: | --- | --- | ---: |
| `a01` | 1,399 / 6,492 | 0/328/0 · 316/12/0 | 25/195/84 | 11 |
| `a02` | 1,316 / 5,754 | 0/32/0 · 22/10/0 | 25/14/3 | 23 |
| `a07` shoreline | 824 / 15,168 | 6/110/0 · 116/0/0 | 13/9/0 | 5 |

No failed roots or destination-color candidates were reported in these
frames. These are view/tick-specific counts of a different scope than the
placement-weighted Warp census. No complete matching texture package was
downloaded locally. See [capture diagnostics](../../retail-matrix.md#lightmaps-on-cpu-rerun)
and [weather behavior](../../effects-weather.md). Physical visible parity,
including a camera with a destination-color subject, still needs review.

## Ordered follow-up: authored descendant census

The census now follows element-200–204 `GfxPath` references through the
pinned GFX pack, with the same member normalization and depth bound as the
player. It weights repeated edges by placement count, counts unreadable,
rejected, unsupported, invalid-path, cycle or depth omissions separately,
and refuses to report a truncated graph when its audit budget is exhausted.
In an authored graph, a cycle is stopped on the first return to its root;
this is not a record of the runtime player's attempted loads.

On the pinned `a01`, `a02`, `a07`, all **368** direct placements were
readable and authored **0 child references**. The prior Warp totals
**96 / 38 / 5** and destination-color zeros therefore also describe those
particular placed text graphs. The new unit fixture checks weighted counts,
case/prefix matching, and bad path/cycle outcomes. See
`12-inventory/authored-graph-census.log`, its input SHA-256 file and test log.

**Remaining:** the pinned graphs do not cover mainland grass, ECM
model-event effects, screen-visible Warp/destination-color subjects, or
post-clip rain fragments. The effect roots/candidates and Clear-vs-Rain
picture deltas above retain their narrower scope. S38/S40 remain correctly
excluded; no physical inventory or visible-parity gate was closed.
