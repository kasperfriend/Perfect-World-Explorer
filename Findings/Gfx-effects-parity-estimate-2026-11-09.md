# Effect parity: what the shipped `gfx.pck` says is done, and what it says is not

The question this file answers is "how much of the retail effect corpus does the
viewer reproduce today, and what would the next fix buy". It is measured, not
estimated by inspection: every `.gfx` member of the shipped archive is read,
every element record is classified against the classes the player can draw, and
every key the loader parses but the player never reads is counted where it is
authored away from its default.

Reproduce the whole census with

```
python3 tools/gfx-parity-census.py google_drive/gfx.pck --json /tmp/census.json
```

`google_drive/` is the staged retail archive, not tracked. The script mirrors
the class sets in `src/gfx_player.rs`; if a class is added there, it is added to
the script in the same commit.

## 1. The corpus

| quantity | count |
|---|---|
| `.gfx` members | 5,459 |
| … carrying at least one element | 5,456 |
| … empty (zero `GFXELEMENTID`) | 3 |
| element records | 31,034 |
| controller records (`CtrlType`) | 76,519, across 5,186 roots |
| roots with at least one controller | 5,186 of 5,456 |

The three empty roots are `gfx/sgc/刺客_致盲_技能击中.gfx` (a `MOXTVersion: 4`
header with no body), `gfx/人物/技能/羽芒/翼击击中.gfx` (header only) and
`gfx/策划联入/人物技能/击中/空.gfx` — the last one is named "empty" and is.

**Correction to the numbers quoted in
`World-effects-2026-11-09.md`.** That pass put the corpus at "76,278 elements".
76,519 is the *controller* count; the element count is 31,034. The per-id
histogram was right in both passes, which is what hid the error — only the total
was wrong, because it summed a different counter. This agrees with
`Gfx-effects-parity-assessment-2026-11-05.md`, which counted 31,019 elements over
5,457 parsed roots (its sweep resolves children recursively and skips the two
unparsable roots; this census globs members and keeps all of them).

### 1.1 Elements by id

Class names are the client's own (`A3DGFXElement::CreateEmptyElement`,
`A3DGFXElement.cpp:598`, tabulated in `World-effects-2026-11-09.md` §1); where the
viewer's internal name differs it is given in parentheses. Counts are this pass's.

| id | class (viewer name) | elements | roots | player |
|---|---|---|---|---|
| 123 | `A3DParticleSystemEx`, ellipsoid emitter | 9,870 | 3,450 | draws |
| 101 | `A3DDecalEx`, 2D screen-space decal | 5,244 | 1,789 | draws |
| 100 | `A3DDecalEx`, 3D decal | 3,542 | 989 | draws |
| 110 | `A3DTrail` (ribbon) | 2,956 | 940 | draws |
| 124 | `A3DParticleSystemEx`, cylinder sampler | 1,748 | 681 | draws |
| 200 | `A3DGFXContainer` (child) | 1,530 | 662 | draws |
| 120 | `A3DParticleSystemEx` | 1,315 | 651 | draws |
| 140 | `A3DGFXRing` (cylinder) | 1,116 | 348 | draws |
| 150 | `A3DGFXLightning` (fractal) | 734 | 158 | draws |
| 130 | `A3DGFXLight` | 546 | 524 | publishes |
| 170 | `A3DGFXSound` | 491 | 469 | counted |
| 160 | `A3DGFXModel` | 447 | 231 | draws |
| 125 | `A3DParticleSystemEx`, sphere emitter | 382 | 139 | draws |
| 121 | `A3DParticleSystemEx`, cone emitter | 320 | 177 | draws |
| 210 | `A3DGridDecalEx` 3D (grid) | 215 | 42 | draws |
| 190 | `A3DParaboloid` (curve) | 208 | 62 | draws |
| 152 | `A3DGFXLightningEx` (trail) | 156 | 46 | draws |
| 102 | `A3DDecalEx`, billboard (crossed pair) | 83 | 33 | draws |
| 180 | `A3DLtnTrail` (wave) | 79 | 51 | draws |
| 122 | `A3DParticleSystemEx` | 40 | 40 | draws |
| 151 | `A3DGFXLtnBolt` (lightning) | 11 | 7 | draws |
| 211 | `A3DGridDecalEx` 2D (grid) | 1 | 1 | draws |

The counts differ by a handful of records from the 2026-11-05 table in
`Gfx-effects-parity-assessment-2026-11-05.md` (element 100: 3,542 here, 3,537
there; 124: 1,748 and 1,739) because that census was built by an earlier reader;
this one re-derives the histogram and sums it to the 31,034 elements above.

No id in the corpus falls outside the decoded set: there is no element class the
loader refuses, and **no** class is parsed without being drawn. This table said
"parsed only" for 210/211 until §47 of the sweep: `emit_clock` routes
`Spec::Grid` to `emit_grid`, which has built one quad per lattice cell since the
waterfall fix, so the census's `PARSED_ONLY` set was stale and the class rows of
this section under-counted coverage by 43 roots and 500 elements.

## 2. Layer one: classes

| | roots | elements |
|---|---|---|
| every element's class is drawn | 5,456 (100%) | 31,034 (100%) |
| needs a 210/211 grid | 0 | 0 |
| holds an undecoded id | 0 | 0 |

No class is left undrawn. The grids this section used to deduct — 215 element
210s and one 211 across 43 roots — are drawn by `emit_grid`, and the census
counts them that way from §47 on.

The controller layer is complete in the same sense: all 13 kinds the corpus uses
(`100..112`, including the `112` shake controller) are parsed by
`src/gfx_file.rs` and dispatched by the player.

| controller | instances | | controller | instances |
|---|---|---|---|---|
| 105 | 16,125 | | 109 | 3,818 |
| 106 | 15,592 | | 110 | 1,918 |
| 104 | 9,463 | | 111 | 1,858 |
| 102 | 6,720 | | 107 | 1,137 |
| 108 | 6,594 | | 103 | 225 |
| 101 | 6,453 | | 112 | 241 |
| 100 | 6,375 | | | |

The three emitter modes that decide where a particle is born are parsed and
consumed: `IsSurface` on 2,674 elements, `3DParticle` on 3,179, `Facing` on
3,111, `IsBind` on 8,269.

## 3. Layer two: the keys the player parses and never reads

A field can be parsed, correct and unread, and still cost nothing — the corpus
decides. The census counts, for each unread key, how many elements author it
away from the client's default, and how many *roots* that reaches. A root counts
as reproduced only when every visual key in it sits at its default.

| tier | roots | elements |
|---|---|---|
| every parsed-but-unread key at default | 2,553 (46.8%) | 10,600 (34.2%) |
| … counting only keys that can change the picture | **4,694 (86.0%)** | **26,166 (84.3%)** |

The gap between the two rows is 2,141 roots, and it is entirely keys that cannot
move a pixel. Each exclusion is sourced, not assumed:

* `MinDist`/`MaxDist`/`IsLoop`/`Force2D` — a sound's rolloff and looping. 469
  roots; no quad.
* `MaxExtent` — the cull extent, and the loader derives it (`1.5 × max(w, h)`
  when zero, `A3DGFXElement.cpp` sprite tail). 1,621 roots; the card is the same
  size either way.
* `NoDownSample` — mip policy. One element.
* `AlphaSeg`/`BetaSeg` — read only by `A3DEllipsoidEmitter::GenAverage`, which
  runs under `if (m_bIsGenAverage)`. 6,641 records write the key, but 6,604
  write the client's default `10` and only 37 (all id 123, the `IsAvgGen`
  population) write anything else. The mode itself is *not* excluded — see §4.
* `Attenuation0`/`FallOff`/`Phi`/`Theta`/`LightType` — the scene-light consumer
  throws them away: `scene_lights.rs:319-322` builds a dynamic light's record as
  `range: 10., stored_attenuation: [0., 0.4, 0.]`, the constants the client
  forces at `0x4250c0`, and the corpus authors `LightType: 1` everywhere with
  `FallOff`, `Phi` and `Theta` at zero. 523 roots author `Attenuation0`; none of
  it survives to the frame.

## 4. Layer three: the 762 roots that are not reproduced, ranked by what a fix buys

5,456 − 4,694 = 762. Forty-three are the grid class (§2). The other 719 carry at
least one visual key the player ignores; the table is the exact per-key count of
unresolved roots, both where that key is the only cause and where it is one of
several.

| key | roots blocked by it alone | roots blocked by it at all | what it is |
|---|---|---|---|
| `DummyEle` | 363 | 399 | the element names another element to take its basis from |
| `ScaleNoOff` | 169 | 178 | gates a keypoint channel the player reads unconditionally |
| `IsAvgGenSeq` | 83 | 124 | sequences `IsAvgGen` over generations |
| `IsAvgGen` | 7 | 38 | the `GenAverage` surface lattice |
| `AvgGenSeg` | 0 | 25 | the lattice's segment count |
| `EnableMat`/`EnableOrgPos1`/`EnableOrgPos2` | 0 | 11 each | ribbon keypoint channels |
| `Pos1Enable`/`Pos2Enable` | 0 | 9 each | fractal keypoint channels |
| `AlphaCmp` | 8 | 8 | an alpha test threshold, unsourced |
| `StrokeAmp` | 7 | 7 | the lightning bolt's amplitude — every one of the 7 lightning roots authors it |
| `2DRender` | 2 | 2 | the root draws in the 2D pass |

The queue is short. Resolving the keys greedily — always the key that recovers
the most still-blocked roots — takes eight steps to reach 99.2% of the corpus,
and every step after the third is fewer than a dozen roots:

| step | key resolved | roots recovered | cumulative | of 5,456 |
|---|---|---|---|---|
| 1 | `DummyEle` | 399 | 5,093 | 93.3% |
| 2 | `ScaleNoOff` | 171 | 5,264 | 96.5% |
| 3 | `IsAvgGenSeq` | 107 | 5,371 | 98.4% |
| 4 | `EnableMat` (+`EnableOrgPos1`, `EnableOrgPos2`) | 11 | 5,382 | 98.6% |
| 5 | `AlphaCmp` | 8 | 5,390 | 98.8% |
| 6 | `IsAvgGen` (+`AvgGenSeg`) | 7 | 5,397 | 98.9% |
| 7 | `Pos1Enable` (+`Pos2Enable`) | 7 | 5,404 | 99.0% |
| 8 | `StrokeAmp` | 7 | 5,411 | 99.2% |

What is left after step 8 is the 43 grid roots and the two roots the 2D pass owns
(`2DRender`); no other rule reaches more than a handful.

`StrokeAmp` is worth naming separately even at 7 roots: it is the *entire*
lightning class, so bolts are the one class where nothing draws as authored.

## 5. What the 86% still does not claim

The number counts roots whose keys are all read, not roots that look right. Three
kinds of gap sit inside it and none of them is measurable by this census:

1. **Rules that are read but not applied.** `camera_shake` is computed per root
   (`Player::camera_shake`, `Walker::camera_shake`) and **added to nothing**: the
   host builds its view-projection in `gpu.rs` and never asks the walker for the
   offset. The 241 `ShakeCam` roots decode correctly and shake nothing. This is
   the cheapest visible win in the whole table — one term in the host's camera.
2. **Classes with an unproven rule inside them.** The particle, ribbon, trail,
   fractal and wave builders each rest on a law that was verified against the
   client's serialiser or binary, but "verified" is per rule, not per effect. The
   known open symptoms are listed in `World-effects-2026-11-09.md` (§40
   waterfall, `水片长`/`水片圆` tile scroll, `怒潮城` portal warp gate).
3. **Composition.** 1,530 `GfxPath` child references, of which 82 point at files
   the archive does not ship (31 distinct authoring mistakes). A root that nests
   one of those is reproduced minus that child, in both the player and the
   client, so it costs nothing against parity with the real game — but it is a
   reason a root can look different from an editor screenshot.

Objects outside the graph are not in these numbers either: the 2,275 `world.ecwld`
placements, the static decal path (`gfx_decal`, whose frame-zero equivalence with
the player is already pinned by the sweep in
`Gfx-effects-parity-assessment-2026-11-05.md`), world lighting, and audio.

## 6. Where the number stands

* Element classes: **100% of roots and of elements**, no unknown id anywhere.
* Controllers: **all 13 kinds, 76,519 instances**.
* Keys: **86.0% of roots** have every visual key read; **46.8%** if audio,
  culling extent, mip policy and the light parameters that the consumer discards
  are counted as if they mattered.
* Largest single unknown: **`DummyEle`** — 399 roots, one rule, and the basis it
  names is already parsed and animated.
* Distance to the class ceiling: **eight rules** (§4's greedy table), of which the
  first three are worth 677 roots and the rest fewer than a dozen each.

The honest headline is the pair, 100% / 86.0%: the loader understands every
record in the archive and every class reaches a draw call, and the renderer is
missing one rule — `DummyEle` — that reaches four roots in ten.
