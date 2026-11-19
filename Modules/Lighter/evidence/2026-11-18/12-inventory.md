# 12 — Authored and run-time omissions are separate

Both `unsupported_version` entries in the [dated catalog](../../scene-catalog-2026-11-18.json)
are accounted for: `s38` and `s40` are Forsaken World 0.525, **ECWLD v14 /
EC-BSD v15**, not the **v12/v12** Perfect World records the viewer supports.
They cannot be admitted by changing the version bytes or using their heights
as a proxy for their scenes. The entries remain unsupported, not counted as
rendered or as broken PW 1.4.2 assets.

## Read-only census on pinned PW 1.4.2 inputs

`cargo run --locked --bin lighter-scene-audit -- ELEMENT/gfx.pck ELEMENT/maps/a01 ELEMENT/maps/a02 ELEMENT/maps/a07`
uses the production scene, GFX container and water readers. It inspects
**direct placed roots only**. It does not resolve child GFX, bind textures,
activate a player or visit model-event owners. Checksums of the obtained
inputs and the raw census are in `12-inventory/` in the [bundle](14-delivery.md).

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
frames only; `a07` and the current build lack a matching retail runtime census.
There are no physical per-map counts in this delivery.

New schema-3 receipts expose `world_effect_roots_*` for playing, queued,
inactive, rejected, unreadable, no package and unsupported in **both**
routes. These count actual world and model-event owners sampled at the
saved frame. They do not make the rest of Native's geometry inventory
exhaustive. Warp counts are the elements omitted by the shared player at
that tick, separately from roots. `Warp:1` requires a framebuffer distortion
pass which neither viewer route implements; it isn't glass or an alpha card.

`world_effect_dest_color_*_candidates` counts emitted world quads before
geometry/budget/depth filtering. D3D source factors 9/10 with destination
factors 1–6 can be composed in linear HDR, not with Native's encoded
attachment clamp. Non-affine pairs (including destination factors 9/10)
are omitted by the ray route; the `effect_draws.unsupported` total includes
other unsupported blend pairs too. These are **not** visibility or parity
counts. Compare matched PNGs if any candidates are present.

`rain_mask_authored` distinguishes decoded `textures/rain.tga` from the
procedural fallback. `rain_at_eye` is selection with the main-eye wet guard;
`ray_rain_cards` counts cards actually inserted into the ray packet (320
when selected, dry and admitted; otherwise 0). On Native, selection is not
a count of surviving fragments. The synthetic fixture pack lacks authored
rain art; its Software Rain run confirms `rain_at_eye=true`,
`rain_mask_authored=false` and `ray_rain_cards=320`
(`12-inventory/rain-fixture/`). We could not load the matching
`textures.pck` on this host, so
retail rain-art availability and Native-vs-ray visible parity are **not
verified**, even though the historical CPU matrix selected Rain. See
[weather behavior](../../effects-weather.md).
