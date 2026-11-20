# Why only the waterfall played: a version whitelist and a coupled failure

**Date:** 2026-11-07
**Fix:** `86cd806`
**Status:** measured over the shipped archives; CI green.

## The evidence that found it

The user's own status line, from the running viewer:

```
FX light producers: 605 loaded, 0 pending, 0 elements, 605 non-light roots,
                    1791 failed, 0 unsupported binary roots;
                    world effects: 2 roots playing, 98 quads, 0 would not build
Failed: world.ecwld (6,7) effect 0: unverified GFX light version
```

**1,791 of 2,396 roots were rejected**, and the reason was printed on every one.
Two roots were playing. That single line explains the whole report — a lone
static waterfall and nothing else — and it is the number the previous three
diagnoses never had.

The `98 quads` also matches: one waterfall sprays a handful of quads, so 98 is
"one waterfall plus one other root", not "the map has no effects".

## Defect 1: an invented version whitelist

`src/gfx_light.rs` gated on a hand-written list:

```rust
if !matches!(version, 37 | 39 | 41 | 45 | 52 | 58 | 64 | 73 | 75 | 79) {
    return Err("unverified GFX light version".into());
}
```

The recovered client does not do this. The root loader `0x6e3470` reads
`MOXTVersion` and rejects anything **above 79** (`cmp 0x4f`) — a ceiling, with no
lower bound, because every optional key is gated per version inside the element
loaders. That is already written down in `src/gfx_file.rs`, which is what makes
this whitelist an invention sitting next to its own refutation.

What the shipped archive actually contains, over all 5,459 text roots:

| version | roots | | version | roots | | version | roots |
|---|---|---|---|---|---|---|---|
| 4 | 1 | | 33 | 381 | | 60 | 106 |
| 28 | 25 | | 34 | 89 | | 62 | 1 |
| 29 | 6 | | 35 | 176 | | 64 | 213 |
| 30 | 216 | | 37 | 631 | | 71 | 1 |
| 31 | 19 | | 39 | 4 | | 73 | 824 |
| 32 | 170 | | 40 | 16 | | 74 | 3 |
| | | | 41 | 88 | | 75 | 11 |
| | | | 43 | 22 | | 76 | 298 |
| | | | 45 | 13 | | 78 | 129 |
| | | | 50 | 2 | | 79 | 1860 |
| | | | 52 | 43 | | | |
| | | | 57 | 3 | | | |
| | | | 58 | 107 | | | |

**27 distinct versions; the whitelist allowed 10.**

Restricted to the 163 roots `world.ecwld` actually names, the damage is exact:

| version | roots | allowed before? |
| --- | --- | --- |
| 30 | 30 | **no** |
| 31 | 2 | **no** |
| 32 | 13 | **no** |
| 33 | 38 | **no** |
| 34 | 2 | **no** |
| 35 | 8 | **no** |
| 37 | 6 | yes |
| 41 | 3 | yes |
| 73 | 9 | yes |
| 78 | 4 | **no** |
| 79 | 48 | yes |

**97 of 163 roots (60%) were refused by a version number.** The named ones are
the fire and the glows — exactly the effects the report said were missing:

| root | version | allowed before? |
| --- | --- | --- |
| `场景/瀑布水花湍急.gfx` | 37 | yes — this is the static waterfall that did play |
| `场景/火焰1.gfx` | 33 | **no** |
| `场景/篝火.gfx` | 33 | **no** |
| `场景/好火焰.gfx` | 33 | **no** |
| `场景/光运.gfx` | 33 | **no** |
| `场景/佛光.gfx` | 35 | **no** |

The grammar is identical across these versions: v33's `火焰1` and v37's
waterfall use the same keys in the same order (`DedaultScale`, `PlaySpeed`,
`GFXELEMENTCOUNT`, `GFXELEMENTID: 120`, `TexFile`, `SrcBlend`, `DestBlend`,
`TexRow`/`TexCol`/`TexInterval`, `Priority`, `ParticleWidth`…). Nothing about the
older roots is unparseable.

## Defect 2: a light failure disarmed the geometry

The whitelist alone would have cost the *lights*. It cost the **geometry**, which
is why nothing was visible. `gfx_owner::load_one` fed the root through the light
parser and treated any error as fatal for the owner:

```rust
match result.and_then(|text| Self::decode(&text)) {
    Ok(emitters) => self.owners[index].state = State::Live(emitters),
    Err(e) => self.owners[index].state = State::Failed,   // <-- dead end
}
```

and `build_one_player` only ever considers `State::Live(_)` owners. So one
parser's opinion about the *light half* silently deleted the sprites, particles,
trails and beams of the *geometry half*.

Those halves are independent. `gfx_light` produces the emitters that publish into
the scene-light grid; `gfx_player` builds the quads that get drawn. The client
does not couple them either — `Draw::update` is reached from `ModelDraw::draw`,
not from the light pass.

## The fix

1. The version gate is now the client's own rule: reject above 79, accept
   everything else.
2. `State::Live` carries `lights` plus an optional `light_error`. A light-side
   failure leaves the owner live, records the reason on the status line, and lets
   the geometry draw. A binary container this reader cannot open (`MOXB`) is
   still a dead end and still reported as *unsupported* rather than as a broken
   asset — that distinction is what
   `binary_roots_are_reported_as_unsupported_not_as_failures` pins, and it caught
   this change when the first cut of it lost the distinction.

## Measured result

All 2,275 placements from `world.ecwld` installed in the real manager:

| | before | after |
| --- | --- | --- |
| roots loaded | 605 | **2,275** |
| roots failed | 1,791 | **0** |
| players built | 2 | **2,275** |
| quads at the waterfall | 98 | **7,945** |

Animation, over 400 frames of the real waterfall: **UVs changed on 390 of 400
frames**, with the spray growing 854 → 3,148 → 8,777 quads. So the texture
scrolling was never broken; it was running on a root nobody could see, and now
runs on all of them.

`non_light_owners` stays 2,275 because these roots genuinely carry no `LightType`
element — only 524 of 10,104 `gfx.pck` members do. They are visual effects, which
is what that counter is defined to mean.

## Why the earlier diagnoses missed it

Each one looked at the pipeline *below* the loader. The records, offsets,
placements, cameras, textures and clocks were all checked and all were correct —
because the owners being examined were the ones that had already loaded. The
rejection happened two stages earlier and printed its reason to stderr on every
run, but nothing in the source reading surfaced the count. The status line the
user pasted is what closed it: `1791 failed`.
