# GFX effects parity assessment: the emitter classes, measured against the retail corpus

This is a follow-up to [the container/runtime write-up](Gfx-effects-reverse-engineering-2026-11-05.md). That document ended with one native behaviour explicitly not copied — "the emitter shape classes were never located in the binary, so box and sphere spawns distribute uniformly inside the authored `AreaSize`/`Radius`". They are located now. This assessment records how, what the client actually does at particle birth, what the retail corpus says about how much that matters, the three places where the viewer was measurably wrong and is now corrected, and the five native behaviours that are measured, documented and still deliberately not reproduced.

Everything below was read out of the inspected `elementclient.exe` (image base `0x400000`, `.text` at RVA `0x1000`, `.rdata` at `0x4d5000`, `.data` at `0x54d000`) and measured over every `.gfx` member of the retail `gfx.pck`: 5,459 roots, 31,019 elements, 13,665 of them particle emitters. Neither the archive nor the executable is committed; the counts and addresses are the evidence trail.

## How the emitter classes were found

The `.gfx` keys do not exist as strings in the binary. Searching for `AreaSize` finds nothing, because the parser never compares key names: the *writer* prints them. What the executable holds is one `"Key: %fmt"` printf format string per key, collected into per-class pointer tables in `.data`:

| Table | Class | Entries |
| --- | --- | --- |
| `0x97d4a8` | keys every element shares | 33 |
| `0x97f1d4` | sprite (100/101/102) | 13 |
| `0x97f284` | particle block A | 20 |
| `0x97f2ac` | particle block B (`ParIniDir` format at `0x97f2cc`) | 10 |
| `0x97f3ec` | average generation (`IsAvgGen`, `AlphaSeg`, `BetaSeg`) | 3 |
| `0x97f130`, `0x97f180`, `0x97f018`, `0x97f074`, `0x97f0b0`, `0x97f428` | misc, ribbon 110, grid 210, child 200, model 160, ref model | — |

From a format string the reader is one xref away, and from the reader the object layout follows: each `sscanf` result is stored at a fixed offset, so the tables give an ordered key→offset map per class. That map is what turned the emitter sub-object from a guess into a known struct.

The classes themselves came from the constructors. Eighty-two vtable-write sites sit between `0x6e0000` and `0x740000`; the one that matters is the emitter factory `0x723ab0`, which dispatches `id − 100` through a six-case jump table at `0x723de4` into a shared base constructor `0x723250` (vtable `0x8edf24`) plus a per-class tail. Every particle element therefore owns an emitter object of ids 100…105 (editor side) and 120…125 (runtime side), allocated at a per-class size with a per-class vtable:

| Case | Ids | Size | Vtable | Constructor tail |
| --- | --- | --- | --- | --- |
| 0 | 100/120 | 0xb4 | `0x8ee0bc` | — |
| 1 | 101/121 | 0xe4 | `0x8ee078` | `+0xb4/+0xb8/+0xbc = 1.0` (the `AreaSize` default the corpus writes 1,361 times) |
| 2 | 102/122 | 0xd8 | `0x8ee034` | — |
| 3 | 103/123 | 0xf8 | `0x8edff0` | `+0xe4 = 0` (`IsAvgGen`), `+0xe8 = 10` (`AlphaSeg`), `+0xec = 10` (`BetaSeg`) |
| 4 | 104/124 | 0xe4 | `0x8edfac` | — |
| 5 | 105/125 | 0xd4 | `0x8edf68` | `+0xc4 = 1.0`, `+0xcc = 0` (`AvgGenSeg`), `+0xd0 = 1` (`IsAvgGenSeq`), plus a sub-object `0x70a600` at `+0x90` |

All six share the same vtable slot meanings, which is what makes the emission maths readable once: slot 1 samples the direction (`0x723440`), slot 2 the lifetime (`0x7237e0`), slot 3 the colour (`0x7237f0`), slot 4 the phase (`0x723a80`), slot 5 the scale (`0x723a20`), slot 6 the rotation (`0x723a50`), slot 10 is shared (`0x724b20`), and slots 7, 8, 9, 11…16 are per class — slot 8 being the birth routine that places a new particle:

| Id | Slot 7 (count) | Slot 8 (birth) | Other per-class slots |
| --- | --- | --- | --- |
| base | `0x892f56` | `0x724ab0` | read `0x723e00`, write `0x724460` |
| 120 | `0x726530` | `0x727230` | — |
| 121 | — | `0x727760` | 9 `0x729670`, 14 `0x7299a0` |
| 122 | `0x7288e0` | `0x7288f0` | 14 `0x7291d0` |
| 123 | — | `0x727e40` | 9 `0x729670`, 11 `0x727c20`, 12 `0x725c90`, 13 `0x725da0`, 14 `0x727c70`, 15 `0x727d70`, 16 `0x7288a0` |
| 124 | — | `0x729b40` | 9 `0x729670` |
| 125 | `0x7256b0` | `0x726570` | 9 `0x727220` (property dispatch `0x7256d0`, jump table `0x725aac`, key index `arg − 0xdb`), 11 `0x726ff0` |

The particle element calls those slots from its own emit routine `0x715b50`, takes the new node from the free list at `+0x18c/+0x194/+0x198`, and links it with the element back-pointers at node`+0x64/+0x68`. The birth routine's output buffer *is* the particle node — the offsets the earlier write-up attributed to a "temp slot" (`+0x00` position, `+0x0c` colour, `+0x10` scale, `+0x24` rotation, `+0x28` velocity, `+0x34` age) belong to the affector loop's 0x3c-byte copy, which is a different struct. On the node, `+0x00` is the age, `+0x04` the TTL, `+0x08` the rotation, `+0x0c` the packed colour, `+0x10…+0x1c` the position in millimetres, `+0x28` the drag term, `+0x44` the unit direction, `+0x50` the mirrored displacement, `+0x60` the speed, `+0x6c` the scale and `+0x74` the bind target.

## What the client does at birth

Element 123 — 9,869 of the corpus's 13,665 particle elements, so the dominant case by a factor of seven — branches three ways at `0x727e40`: `IsAvgGen` (emitter`+0xe4`) to `0x728420`, `IsSurface` (emitter`+0x84`) to `0x727f00`, and otherwise to the ordinary shape routine `0x728270`. That routine is the one the viewer has to match:

```
728 270:  three draws of rand() * (2/32767) - 1     ; 0x8e6efc = 2/32767
          retry while x*x + y*y + z*z > 1            ; rejection sampling
          out+0x10/0x14/0x18 = M * (x, y, z)         ; M at emitter+0xc0..+0xe0
          if (!IsBind) out += emitter+0x68           ; the emitter position
          if (IsDrag)  out+0x28 = emitter+0x68       ; 0x86, DragPow at +0x88
          out+0x34 = emitter+0xa0                    ; the composed orientation quaternion
          vtable[1](out+0x44)                        ; slot 1 = the direction
```

The matrix `M` is built by slot 9, `0x729670`, which ids 121, 123 and 124 share:

```
729 670:  if (emitter+0x85)                          ; IsBind
            row0 (+0xc0..+0xc8) = global(0x9c0358) * (AreaSize.x * 0.5)
            row1 (+0xcc..+0xd4) = global(0x9c0348) * (AreaSize.y * 0.5)
            row2 (+0xd8..+0xe0) = global(0x9c0338) * (AreaSize.z * 0.5)
          else
            the same three rows scaled by the basis handed in by the caller
```

`AreaSize` lives at emitter`+0xb4/+0xb8/+0xbc` (the constructor default of `1.0, 1.0, 1.0` for id 121 is the corpus's second most common value), the 0.5 comes from `0x8d5894`, and the three globals at `0x9c0338/0x9c0348/0x9c0358` are the world axes — initialized at run time, so their values are not in the file, but their role is fixed by the ordering. **The emission volume is therefore an ellipsoid with semi-axes `AreaSize/2`, sampled uniformly by volume, not a box of half-extents `AreaSize/2`.** Id 125 is the only class with a `Radius`, and its property dispatch writes a scalar at `+0xc0` rather than a matrix.

The direction sampler, slot 1 at `0x723440`, is shared by all six classes:

```
723 440:  basis = IsBind ? global(0x9c0338) : f(quaternion at +0x74..+0x80)
          if (Angle at +0x10 == 0.0 at 0x8d58a0) return basis
          alpha = rand() * (1/32767) * Angle         ; 0x8e6f10, no clamp
          beta  = rand() * (2*pi/32767)              ; 0x8e6f14
          two successive quaternion rotations through 0x7b6fd0
```

So the polar angle is uniform in `[0, Angle]` and the azimuth uniform in `[0, 2π)`. The lifetime, colour, scale and rotation slots are equally plain: `0x7237e0` draws the TTL, `0x723a20` and `0x723a50` lerp scale and rotation between their authored minima and maxima, and `0x7237f0` returns the shared colour when `ColorMin == ColorMax`, otherwise lerps four independent draws channel by channel and truncates each through the float-to-int helper `0x892e78` — with the alpha channel additionally multiplied by emitter`+0x8c` before its truncation.

## What was wrong, and what changed

Three of the viewer's emission rules disagreed with the above. All three are corrected in `src/gfx_player.rs` and each is now pinned by a test, because in every case the difference is distributional and a single sample cannot show it.

**1. `AreaSize` was read as a box.** `spawn` emitted `rng.signed() * area * 0.5` per axis — a box of half-extents `AreaSize/2`, corners included. The client emits an ellipsoid of the same semi-axes, so every corner particle the viewer produced is a particle the client never produces, and the viewer's volume is `8/((4/3)π) ≈ 1.91` times too large. This affects **11,928 elements** (id 121: 320, id 123: 9,869, id 124: 1,739), which is 87% of all particle elements in the archive. `AreaSize` is isotropic in the great majority of records — the corpus's most common triples are `(0.1)³` 1,457 times, `(1)³` 1,361, `(0.2)³` 840, `(0.5)³` 835, all-zero 504 — which is why the bound/unbound distinction in `0x729670` (world axes versus the caller's basis) rarely shows: an isotropic ellipsoid is invariant under the element's rotation. It is now `random_unit_ball(rng) * area * 0.5`, with the ball sample shared with the `Radius` path so both are uniform in volume. Test: `area_size_spawns_inside_its_ellipsoid` (4,000 spawns against an anisotropic `(2, 1, 0.5)` volume, asserting the ellipsoid inequality and that the semi-axes are actually reached) and `unit_ball_samples_fill_the_volume` (mean radius 3/4, which is what separates a volume sample from a surface one).

**2. The cone was sampled by solid angle and clamped to `[0, π]`.** `cone_direction` drew `z` uniform in `[cos(Angle), 1]`, which is uniform in solid angle, and clamped the angle first; the gate in `spawn` was `angle > 0.`. The client draws the polar angle uniform in `[0, Angle]` and never clamps. The corpus needs the native reading on both counts:

| `Angle` value | Elements | Client | Old viewer |
| --- | --- | --- | --- |
| `0` | 9,719 | straight along the basis | same |
| `6.283185` (2π) | 2,130 | whole sphere, uniform in polar angle | whole sphere, uniform in solid angle |
| `3.141593` (π) | 248 | whole sphere, polar angle axis→anti-axis | whole sphere, uniform in solid angle |
| `0.523599` (30°) | 416 | 30° cap, denser at the rim | 30° cap, denser at the pole |
| `0.052360` (3°) | 256 | narrow cap | narrow cap, different density |
| `0.349066` (20°) | 131 | 20° cap | 20° cap, different density |
| negative | 36 | cap mirrored through the emitter | collapsed onto a single ray |

A half turn and a full turn cover the same directions — the full turn folds `(π + t, β)` onto `(π − t, β + π)` — but both distribute by angle, not by solid angle, which puts visibly more particles near the axis and its opposite than the viewer did. The 36 negative angles were the hard failure: `angle > 0.` sent them down the `else` branch, so those emitters fired a single ray instead of a mirrored cone. The gate is now `angle != 0.` and the sample is `(sin α cos β, sin α sin β, cos α)` with `α = u₁·Angle`, `β = u₂·2π`, drawn in the client's order. Tests: `cone_polar_angle_is_uniform_in_the_angle` (half of a 90° cone's samples must fall inside 45° of the axis, where a solid-angle sample puts 0.293) and `cone_angle_is_never_clamped` (30° stays inside 30°, a full turn reaches behind the axis, a negative angle opens a mirrored cone rather than a ray).

**3. The colour sample rounded.** `random_color` computed `min + (max − min) * u` and rounded it. The client computes `min * (1 − u) + max * u` and truncates through `0x892e78`. The two differ by up to one unit per channel on almost every draw, and the truncated form is the same convention the verified static path already uses for alpha (`gfx_decal.rs:440`) and that controller 105 uses for its per-channel steps. Both the expression and the truncation are now the client's. Test: `color_channels_truncate_instead_of_rounding`, which replays the draw sequence and asserts equality with truncation *and* inequality with rounding, so the assertion has teeth.

Nothing else in the emission path moved. The draw counts per spawn are unchanged (three for a ball sample, two for a cone draw), so the player stays deterministic under a fixed seed and the corpus sweep stays comparable.

## Measured, documented, deliberately not reproduced

Six native behaviours are described below. Five are understood well enough to describe and not well enough to copy honestly; the sixth, `IsSurface`, is now reproduced for the two emitter classes whose samplers are decoded and still described for the other two. Each is recorded in the source comments where the viewer diverges. A seventh, `IsUseHSVInterp`, was reproduced on 6 November from the binary alone; its entry is kept below and marked, and the decode is in *The HSV colour path, reproduced*.

**`ParIniDir` is three Euler angles, not a direction vector.** The parser at `0x7242e2` reads the key's `%f,%f,%f`, passes the three floats to `0x7b6f10` — which halves each argument by the 0.5 at `0x8eff98` and takes sines and cosines of the halves, the `sin(a/2)cos(b/2)…` expansion of three successive rotations, i.e. Euler→quaternion — then rotates the quaternion already at emitter`+0x74…+0x80` by it and stores four floats at `+0xa0…+0xac`, which the birth routine copies into node`+0x34`/`+0x50`. The corpus agrees with the reading: the only non-zero values are exact degree conversions (`0.017453` = 1°, `1.570796` = 90°), which no unit direction would be, and about a hundred of the 13,665 particle elements author one at all (9,326 write zeros). The viewer still normalizes `ParIniDir` as a vector in `emission_axis`, which is wrong for those hundred and right for the rest. Reproducing it needs the provenance of the quaternion at `+0x74`, which is written outside the parse path and was not recovered; guessing it would trade a known small error for an unknown one. *(Superseded 6 November — the provenance is recovered, so the composition is a reading rather than a guess: emitter`+0x74` is `Q ⊗ keypoint Direction` (`0x71a4dc`, with `Q` from `0x6fa810`, root`+0x1a4` or `+0x1b4`), and `0x71b450` recomposes `+0xa0` from it, both at spawn and again at `0x716040` after the spawn loop's `0x716025`. The two quaternions that loop combines are the element's cached orientation at `+0x21c`, saved at `0x71a56c`–`0x71a57b` from the emitter's own `+0x74`, and the record `[esp+0xb8]`, through `0x7b7010` — a quaternion slerp, not a plain blend: `|dot| > 0.75` takes the linear path, otherwise `sin((1−t)θ)/sinθ`, with a negated copy of `b` when `dot < 0`, so it always interpolates the short way round. What is still missing to reproduce `ParIniDir` is narrower: how slot [1], `0x723440`, turns the four floats at emitter`+0xa0…+0xac` into the direction it writes at node`+0x44`. See "The emitter object" in the reverse-engineering document.)* *(Resolved 6 November, and the answer changes the entry's conclusion: slot [1] never reads `+0xa0` at all. `0x723440` writes node`+0x44` as the global Z axis `0x9c0338` when `IsBind` is set (`0x723451`) and as `rotate(Z, quat(emitter+0x74))` when it is not (`0x72345b`–`0x7234ff`), then optionally tilts it inside a cone whose axis is X or `rotate(X, q)`. So `ParIniDir` reaches only node`+0x34`, the particle's initial orientation, and the viewer's `emission_axis` is wrong twice over: it normalizes `ParIniDir` as a direction for the ~100 elements that author one, and its fallback axis is the element's **Y** where the client's is **Z**, which is every element that authors none. See "The direction slot `0x723440`" in the reverse-engineering document.)* *(Both are fixed — see "The emission direction, reproduced". What is left of this entry is the orientation half: node`+0x34` is a quaternion, and the player models particle orientation as a 2D spin, so `ParIniDir` stays parsed and unused rather than misused.)*

**`IsUseHSVInterp` selects a second colour path.** *(Superseded 6 November: this is no longer a divergence, it is implemented; see the section below. The original measurement is kept as written.)* `0x7237f0` tests emitter`+0xb0` at `0x72380c` and branches to `0x723812`, which lerps the HSV copies at `+0x38…+0x44` and `+0x48…+0x54` per channel — hue wrapping at ±1.0 — and converts back through `0x7898f0` (HSV→RGB). Those copies are made at parse time by `0x724422…0x724444`, which always runs the authored RGB through `0x789ad0` (RGB→HSV, unpacking ARGB and scaling by `1/255` at `0x8f0928`). **168 elements** ask for it (id 123: 161, id 124: 4, id 125: 3). The viewer lerps in RGB for all of them; the difference is a hue-path artefact on interpolated colours, visible as a grey band where HSV would sweep through the hue.

**Emission count is detail-scaled.** Two gates reduce how many particles the client spawns, and the viewer applies neither, which means it always renders at maximum detail:

- `0x715b88…0x715bfd`, inside the emit routine: when the flag at root`+0x2a0` is set, the count is multiplied by `(root+0x298 * global@0x9c1318) / root+0x29c` and truncated, but only if that factor is below 1.0 and the divisor at root`+0x29c` passes a range check against the two constants `40.0` (`0x8edaac`) and `10.0` (`0x8edaa8`) — above 40 always, between 10 and 40 only when the count exceeds 3, at or below 10 never. *(Superseded 6 November: the factor **is** recoverable. `0x9c1318` has exactly one writer in the whole binary — the three-instruction initialiser `0x715150`, which multiplies the 10.0 at `0x8edaa8` by the 0.8333333 at `0x8edab0` and stores **8.3333333**. It is a compile-time constant product, not runtime state, so the factor is `(root+0x298 × 8.3333333) / root+0x29c`, and both thresholds read out of `.rdata` as 10.0 and 40.0 exactly. What remains unknown is where root`+0x298`, `+0x29c` and the `+0x2a0` flag come from: no instruction anywhere in `0x700000`–`0x730000` writes them, so they belong to whatever owns the root object, and the paired writes at `0x484092`/`0x484098` — the nearest candidate — sit in another subsystem. The shape still reads as a distance denominator against a particle-detail setting.)*
- `0x70ba00`, the element update: `Priority` (element`+0xdc`, constructor default 4 at `0x6f8fdb`, values 1…4 across ~30,000 corpus records) is compared against a detail threshold from the settings object (`call 0x41a3c0`, threshold at `+0x134`) and the element is skipped when it loses (`0x70bd0b`). `Priority` is a level-of-detail gate, not a sort key. *(Read in full 6 November: `0x70ba00` first calls `0x6fa670` on its argument and returns false when that says no (`0x70ba13`–`0x70ba20`); it then tests the byte at element`+0x12e` and skips the comparison entirely when it is clear (`0x70ba2b`). Only after both does it fetch the settings object, compare `Priority` against `+0x134` (`0x70ba42`, signed greater wins) and, when `Priority` loses, fall through to a second test on element`+0x50` whose negative case is the one that returns false (`0x70ba4f`). So a higher `Priority` survives a lower detail setting, and losing the comparison is not by itself a cull. Completed the same day by dumping the jump target: `0x70bd0b` is `mov $0x1,%al; pop %esi; add $0xac,%esp; ret $0x4`, so it **returns true**, and the branch at `0x70ba44` is `jg` on `Priority > detail`. A high priority therefore skips everything after the comparison — including the only `return false` on that path, the visibility test `0x6fe120` on the node at element+0xa0, whose failure at `0x70ba91` jumps to the `xor %al,%al; ret $0x4` block at `0x70ba17` — and it also skips the distance scalar `0x6fe060` builds from element+0x58 and element+0x90 into `[esp+0x24]`, which `0x70bae7` hands to root+0x2a2's consumer. **A higher `Priority` is exempt from culling, not a candidate for it.** That makes the viewer's own light gate a divergence: `gfx_light::Emitter::tick` suppresses publication when `priority > manager_priority`, and the comment above it cites `0x70ba00`, which is the opposite sense. It is left as shipped and named here rather than flipped, because the native rule's second half is a camera-relative visibility test that emitter is not given; `gfx_catalog::priority_exempts` carries the dumped direction for whoever adds it. The same `0x6fe060` call is also the strongest lead on the emission LOD's divisor: it takes a position at element+0x58 and leaves a float that element+0x90 is added to, which is the shape of a camera-relative distance, and root+0x29c is a distance. The engine's detail keys are on disk: `configs.pck` ships `configs/angelica.cfg`, whose `[a3d]` section carries `detail_scene`, `detail_model` and `detail_terrain`, all 1 in the shipped file, alongside `dynamic_light`, `high_quality_sky`, `high_quality_tex`, `trilinear` and `num_t2_block_grid = 32`.)*

The rest of the client's data set is reachable now, which changes what counts as blocked. The Drive folder `pw142` holds the full installation, including `element/` with fifteen `.pck` archives in the same v2 directory format `src/pck.rs` reads: `models` 888 MB, `litmodels` 397 MB, `building` 326 MB, `sfx` 273 MB, `loddata` 252 MB, `textures` 117 MB, `gfx` 97 MB, `surfaces` 93 MB, `facedata` 25 MB, `trees` 20 MB, `grasses` 12 MB, `fonts` 4.6 MB, `shaders` 4.4 MB, `interfaces` 3.2 MB and `configs` 588 KB, plus `elementclient.exe`, `elementskill.dll` and the `maps`, `data` and `music` folders. `configs.pck` is enumerated and read here; `loddata.pck` is the obvious source for the terrain heights `MatchGrnd` and `IsStartOnGrnd` need, and `models.pck` for anything that still wants the owner's triangles.

**`IsSurface` needs the owner model.** *(Corrected 6 November: the sampler reads no model at all, so the interpretation and the blocker do not stand — see the next entry. The divert address and id 120's `π/2 − Angle` stand. The element counts were re-measured later the same day and the id-124 figure below is 9 short: see the census note.)* `0x727e40` diverts to `0x727f00` when emitter`+0x84` is set, which places particles on the surface of the bound model and needs its triangles. **2,674 elements** author it (id 123: 1,520, id 124: 1,045, id 120: 86, id 121: 11, id 125: 12). *(The 2,665/1,036 figures recorded earlier came from a raw text census that undercounted id 124 by nine; two independent counts now agree — the parser, walking every corpus element, and a fresh `GFXELEMENTID`-split scan of the archive text, which also finds 13,675 `IsSurface` keys across ids 120 to 125 and no value other than 0 and 1. The other per-id counts are unchanged.)* The viewer has the owner's model queue but no triangle sampling path into the emitter, so these particles spawn from the element's ordinary volume instead of the mesh. Id 120's surface block also uses `π/2 − Angle` (`0x8ee228`) rather than `Angle`, a second reason not to approximate it.

**Surface emission is the emitter's own ellipsoid, and it is not blocked on the model.** Every memory operand inside id 123's surface sampler `0x727f00` is based on `%esp` (88 of them), `%esi` (16, the emitter) or `%edi` (8, the `rand` IAT slot it calls twice). There is no third object, no vertex buffer and no triangle list. The only emitter fields it reads are the nine basis floats at `+0xc0…+0xe0` — the same matrix the volume sampler `0x728270` multiplies by, and neither routine reads `AreaSize` at `+0xb4…+0xbc` directly. So "surface" means the surface of the emitter's own `AreaSize` ellipsoid — for ids
123 and 124; see the correction below, id 120's surface block never reads
`AreaSize` at all. What was missing then was narrower than a model dependency: the vector library the sampler is written in. `0x401ca0` and `0x401cc0` construct and convert, `0x4093d0` and `0x4071f0` are the two binary operations that build a direction out of a quaternion and a global axis, `0x633700` is called with 2.0 and with a doubled quaternion component, and `0x401d10` once. *(All eight are decoded now — see the next entry and the library table in the reverse-engineering document.)* The routine's shape is readable without them: two `rand()` draws, each scaled by 2π/32767 (`0x8e6f14`), each turned into a quaternion by `0x7b6fd0` about the globals `0x9c0348` and `0x9c0358`, combined into a direction, then multiplied into the three basis vectors at `+0xc0`, `+0xcc` and `+0xd8` alongside a negated copy.

**Id 123's surface sampler is decoded, and reproduced.** *(6 November, later the same day.)* With the vector library decoded, `0x727f00` reads end to end. It spends two `rand()` draws. Each is scaled by 2π/32767 (`0x8e6f14`) and turned into a quaternion by `0x7b6fd0`; the first is about the global axis at `0x9c0348`, and the cross/scale/add chain that follows evaluates `u + 2w(v x u) + 2(v x (v x u))`, which is the rotation of the global axis at `0x9c0358` by that quaternion. The second draw rotates `0x9c0348` about the first rotation's own output (`0x728004` takes it as its axis argument), which sweeps the sphere uniformly. Call the result `D`.

The position is a linear combination, not a component-wise product: the three `0x401ca0` constructions at `0x7280ff`, `0x728132` and `0x728165` build `D.x * basis0`, `D.y * basis1` and `D.z * basis2` from the nine floats at `+0xc0…+0xe0`, and `0x7281d5` and `0x728202` sum them into `node+0x10`. Since the bound basis builder `0x729670` makes those vectors `AreaSize * 0.5` along the three axes, the sample lands exactly on the ellipsoid — the same matrix the volume path multiplies a unit-ball sample by, which is why the two differ only by the sample's radius. The direction written to `node+0x44…0x4c` (`0x7281ad` through `0x7281b3`) is the negated `D` built by the `fchs` chain at `0x72816a`: the sampler emits inward along the radius, and because id 123's birth flow calls the TTL slot next rather than the direction slot, the emission cone never applies to a surface particle. The tail is the volume path's tail: `+= emitter+0x68` unless `IsBind` (`0x728216`), `IsDrag` copies the emitter position to `node+0x28`, and `node+0x34` takes the four floats from emitter`+0xa0` — which are the composed orientation quaternion, not a colour *(corrected; see "The emitter object" in the reverse-engineering document)*.

The bound basis pairs `0x9c0358` with `AreaSize.x`, `0x9c0348` with `.y` and `0x9c0338` with `.z`, which identifies the two globals as the x and y axes without needing their run-time values. The viewer now reproduces this for id 123 (`surface_direction` in `gfx_player.rs`, five tests: the ellipsoid equation holds with equality, the direction negates the sample, the samples cover the sphere in both signs on every axis, exactly two draws are spent, and another class keeps the volume path).

**Verified against the archive.** Two corpus tests now live in `gfx_player.rs` and skip when the archive is absent, so CI is unaffected and the harness survives the loss of a scratch directory. *(There are four now: this one, id 124's cylinder test, the whole-corpus sweep, and the basis-scalar test in "The unbound emission basis, reproduced".)* The first walks all 5,459 members, finds the same **1,520** id-123 surface elements the census counted, and checks each against its own authored `AreaSize`: the direction is unit length, the position equals `-direction * (AreaSize * 0.5)` component-wise, and where no extent is zero the ellipsoid equation holds with equality. **94 of the 1,520** author a zero extent, which collapses the ellipsoid to a plane or a line, and that is why the invariant is written component-wise instead of as a division by the semi-axes; none author a negative extent and none omit `AreaSize`. The second plays all 5,457 parseable files for 120 frames of 16 ms with children resolved from the same archive: **34,416,130 quads, none of them non-finite**, 1,151 files reaching completion, and the same **64 distinct warnings** as the earlier sweep. The quad count is not comparable to the 19,861,420 recorded above, which a since-lost harness produced with frame parameters that were never written down. It is not invariant under a sampler change either: adding id 124's cylinder took this harness from 34,416,130 quads to 34,416,109, because a sampler that spends a different number of draws shifts the stream feeding every later random value in the same element. The parsed, played and distinct-warning counts are comparable, and they match.

**Id 124's surface is a cylinder, and it is reproduced too.** *(6 November.)* Id 124's `0x729be0` opens with the identical call chain — draw, `0x7b6fd0`, cross, copy, cross, copy, two scalings, two adds — but about `0x9c0338` rather than `0x9c0348`, and the second half is different in kind. Its second draw is not an angle: `0x729cc4` scales it by `2/32767` (`0x8e6efc`) and `0x729cca` subtracts `1.0` (`0x8d5c28`), giving a uniform in `[-1, 1]`, and there is no second quaternion at all. The three basis products at `0x729cd2`, `0x729cf6` and `0x729d1c` therefore take their coefficients from the rotated vector's **x and y only** — its z is zero and never read — with the raw second draw standing in for the third. Since the bound basis pairs `0x9c0338` with `AreaSize.z`, the sample is `(cos θ · Ax/2, sin θ · Ay/2, u · Az/2)`: the lateral surface of the cylinder the extents describe, sweeping a full turn around z and the whole height, rather than id 123's ellipsoid.

Its direction differs too. The sampler writes only node+0x10/0x14/0x18, the `IsDrag` copy at node+0x28 and the colour at node+0x34 — never node+0x44 — and the birth flow at `0x729bb9` calls the direction slot `vtable[1]` with `&node+0x44` after *both* samplers, then the TTL slot. Id 123's flow at `0x727ed4` goes straight to the TTL slot. So a surface particle keeps the emission cone on id 124 and loses it on id 123, which is exactly the kind of per-class divergence that made borrowing one formula for the other a guess worth not making. The tail contract is otherwise the same: position `+= emitter+0x68` unless `IsBind` (`0x729d83`), and node`+0x34` from emitter`+0xa0` (the composed orientation, as above).

Id 124 is implemented as `cylinder_sample` with four tests, including a corpus test over the archive's **1,045** id-124 surface elements: the cross-section sits on the ellipse to within 1e-3 and the height stays inside the extent, with **341** of them authoring a zero extent, which collapses the cylinder onto an axis or a plane and is skipped for the cross-section check. That leaves ids 120 and 121 — **97 of the 2,662 elements that matter**.

*Corrected 6 November, after decoding id 120's surface block.* The sentence above
it that generalises "surface means the surface of the emitter's own `AreaSize`
ellipsoid" is true of ids 123 and 124 and **false of id 120**. Id 120's block
(`0x7272aa`–`0x727615`, inline in the birth routine `0x727230`) spends one `rand()`
draw on an azimuth about the global Z, turns that into a spun X axis, builds a
quaternion about it from `π/2 − Angle` (`0x8ee228` minus emitter`+0x10`), composes
it with the orientation quaternion at emitter`+0xa0…+0xac`, rotates the global **Y**
axis by the result into **node`+0x44`** — the direction — and then sets the position
to that direction times the float at `0x8ee224`. It never reads `AreaSize` at
emitter`+0xb4` or the basis at `+0xc0…+0xe0`. `0x8ee224` is 0.001, and the volume
path multiplies by the same constant twice (`0x7276bc`, `0x7276d6`), so it is the
engine's unit conversion on node positions rather than a radius: **id 120's surface
particles sit one file unit from the emitter, with the spread in their direction
rather than in their position.** The viewer *got* those 86 elements wrong
in kind, not merely in shape — it put them on the `AreaSize` ellipsoid with the
shared direction slot, where the client puts them one file unit out along a cone of
half-angle `Angle` around its local **Z** axis. It does not any more: `spawn` has an
id-120 arm now, and id 121's 11 were the last gap, closed by `box_surface_sample`
— so all **2,662** `IsSurface` elements that the client acts on are reproduced. *(Corrected the same day: the
half-angle given here first was `π/2 − Angle` around Y — that is the intermediate
tilt the block applies to the global Y axis before composing, not the cone that comes
out.)* The operand order and the `IsBind` split are both read now: node`+0x34` is
`q2 ⊗ q1` with q1 = `axis_angle(Z, θ)` and q2 = `axis_angle(rotate(X, q1), π/2 −
Angle)`; the unbound side composes `Q ⊗ cone` against emitter`+0xa0…+0xac` while the
bound side skips it (`jne 0x727552`); and the direction written to node`+0x44` is
`rotate(Y, node+0x34)`, which reduces to `(−sin Angle·sin θ, sin Angle·cos θ, cos
Angle)` — checked numerically to 3e-16. Every particle sits at polar angle `Angle`
exactly, so this is the surface of the cone rather than the filled cone the shared
direction slot produces. These 86 are reproduced now: `surface_cone` walks the same
chain and spends the same single draw, `to_emitter_local` factors out the frame step
`emission_axis` already did inline, and the position is that same vector one unit
out, exactly as node+0x10 and node+0x44 are in the client.

Id 121's surface sampler `0x727800` (11 elements) is read now too, and it is
neither a rejection loop nor the opposite shape — it is a **box surface**, and the
two `fmuls` at `0x72780d` and `0x72782f` that looked like a squaring of `AreaSize`
are the face products `x·y` and `x·z`. `0x72780d`–`0x727843` form all three
products and their sum; one draw picks a face, below `xy/sum` the Z faces
(`0x727868`), below `yz/sum` the X faces (`0x7278df`), otherwise the Y faces; a
second draw against the 0.5 at `0x8d5894` picks that face's sign, the `-1.0` at
`0xbf800000` flipping to `+1.0` at `0x3f800000`; and two `2/32767 − 1` draws fill
the other two coordinates. Which slot each of the three lands in is what
distinguishes the branches (`0x7278af`/`0x7278c9`, `0x727924`, `0x727977`). The
tail `0x727991`–`0x727a41` multiplies them by the basis rows at emitter`+0xc0`,
`+0xcc` and `+0xd8` into node`+0x10` and writes no direction, so id 121's birth
flow still calls the direction slot and the cone applies. Four draws per particle,
in every branch. It is reproduced as `box_surface_sample`, and unbound particles
get the emitter position added (`0x727a6c`), node`+0x34` filled from
emitter`+0xa0` (`0x727a90`) and the drag anchor at node`+0x28` when `IsDrag` is
set (`0x727a5a`) — all three of which this player already did for the other
classes.

The face choice carries a quirk that only a test would have caught. Because the
second comparison is against `yz/sum` alone and not the cumulative
`(xy+yz)/sum`, the X faces need a draw that is simultaneously at least `xy/sum`
and below `yz/sum`, which is possible only when `AreaSize.z > AreaSize.x`. A cube
asks for a draw both `>= 1/3` and `< 1/3`, so **a cube never emits from its X
faces**: a third of its samples land on Z and two thirds on Y. With half-extents
(1, 0.5, 0.25) the areas would give Z 4/7, X 1/7, Y 2/7 and the client gives Z 4/7,
X none, Y 3/7 — the X faces' share falls to the Y faces. First measured as a
failing assertion (`a face was never chosen: [0, 825, 1175]`), then pinned both
ways by `id_121_face_choice_carries_the_clients_skew`. Id 121's `0x727800` calls none of the vector helpers at all — scalar x87 throughout — and spends four draws per particle rather than the ten `call *%edi` sites its body contains; id 120's block at `0x7272aa` runs the same chain but adds a call to `0x70a320` and uses `π/2 − Angle`. Id 120's block is read in full now (`0x7272aa`–`0x7275d6`): one uniform draw, a rotation about the global Z axis `0x9c0338` — *(corrected: this said the vector part of the emitter's composed orientation at `+0xa0`; the dump shows `push $0x9c0338` at `0x7272cc`, and `+0xa0` is composed in later and only when unbound)* — (`0x7b6fd0` at `0x7272d1`, then the Rodrigues expansion spelled out with `0x401ca0`, `0x4093d0`, two `0x633700` and two `0x4071f0`), a second angle `π/2 − Angle`, which is not a draw at all (`0x8ee228` at `0x72737b`, the subtraction at `0x727381`, `0x7b6fd0` again at `0x727395`), and both handed to `0x70a320` at `0x727448` — which takes four floats, stores them at `this+0xc/+0/+4/+8` and returns with `ret $0x10`, so the block composes a *quaternion*, not a position. A scan of the whole block finds no read of `+0xc0…+0xe0`, which the vtable table now explains rather than leaves anomalous.

The per-class split is measured now too. Id 121 diverts to `0x727800` (volume `0x727ab0`), id 123 to `0x727f00` (volume `0x728270`), id 124 to `0x729be0` (volume `0x729df0`), and id 120 inlines its surface block at `0x7272aa`. Id 125 authors `IsSurface` on 12 elements but its birth routine `0x726570` never reads `+0x84`, so those 12 are inert; the 2,662 that matter are ids 120, 121, 123 and 124. *(Corrected: they do not all read the same basis. Reading the seven emitter vtables out of `.rdata` shows slot [9] is the basis builder `0x729670` for ids 121, 123 and 124, but `0x56fbb0` — an engine routine outside the gfx region — for ids 120 and 122, so id 120's class never builds an `AreaSize` basis and its samplers cannot read one.)*

**The unbound emission basis is rotated, and the viewer's is not.** `0x729670`, the routine that fills the basis both samplers read, branches on `IsBind` at emitter`+0x85` (`0x72967e`). Bound emitters get `diag(AreaSize * 0.5)` laid against the three global unit axes — `0x9c0358` with `AreaSize.x` into `+0xc0…+0xc8`, `0x9c0348` with `.y` into `+0xcc…+0xd4`, `0x9c0338` with `.z` into `+0xd8…+0xe0` — which is what `spawn` reproduces. Unbound emitters take `0x729763`, which multiplies a single scalar argument into all three `AreaSize` components (the `push`/`fstps (%esp)` idiom shifts the frame, so the reads at `0x74(%esp)`, `0x7c(%esp)` and `0x80(%esp)` are the same slot read three times), builds a quaternion from emitter`+0x74…+0x7c` at `0x7297ad`, converts it to a matrix — `0x7297f4` doubles its w, the first term of that conversion — and writes the same nine slots at `0x72987c`. The unbound emission volume therefore carries the emitter's orientation and the viewer's does not. It is the same blocker as `ParIniDir`. *(Superseded 6 November, fully: both unknowns are recovered and the scalar is shipped. Emitter`+0x74` is written at `0x71a4dc` as the Hamilton product `Q ⊗ keypoint Direction`, with `Q` selected by `0x6fa810` from root`+0x1a4`/`+0x1b4`; `0x716025` is a second, later writer in the element's spawn loop, which also lerps the emitter's position `+0x68` per spawned node. The scalar argument is keypoint `Scale` × root `PlaySpeed`, read at `0x71a35b`/`0x71a501`. See "The unbound emission basis, reproduced".)* 8,259 of the corpus's 13,665 particle elements author `IsBind`, leaving 5,406 on the unbound path.

**Ground snapping at spawn needs terrain.** `0x715cee` gates a call to `0x41a430` with the position at node`+0x10` and writes the returned height into node`+0x14`, when `(IsBind && element+0x1b5) || element+0x23e`. The viewer's player has no terrain handle at effect level, so `MatchGrnd`/`IsStartOnGrnd` remain parsed and unused for particles (the static decal path does use ground matching, through a different route).

Two smaller divergences are noted in the source rather than fixed: the alpha factor at emitter`+0x8c` that `0x7237f0` folds into the alpha channel is the element's live alpha, which the player already applies once while building the quad, so applying it at spawn as well would double-count it; and id 125's average-generation sequence (`IsAvgGen` on 373 of its 382 elements, `AvgGenSeg`, `IsAvgGenSeq`, dispatched at `0x7256d0` through the jump table `0x725aac`) plus id 123's `IsAvgGen` branch to `0x728420` (45 elements) are counted but not decoded — `AlphaSeg`/`BetaSeg` (6,641 records, 6,604 of them the constructor default 10) belong to the same family.

## The unbound emission basis, reproduced

*(6 November, third pass.)* The entry above recorded the unbound basis as a divergence blocked on two unknowns: the value of the scalar argument, and the provenance of the quaternion at emitter`+0x74`. Both are recovered, and the part that changes the image is shipped.

**The caller, and the scalar.** Vtable slot [9] has exactly two call sites, both in the element update `0x71a1e0`, and they are the two `IsBind` arms: `0x71a351` reads emitter`+0x85`, `0x71a359` skips the call when it is zero, so the bound arm calls the slot at `0x71a369` and the unbound arm at `0x71a50d`. Both pass one float, computed identically at `0x71a35b` and `0x71a501` as `[esp+0x48] × [esp+0x24]`. Slot `0x24` is filled at `0x71a225` from `[element+0x10]+0x1ec` — the root's `PlaySpeed`, the same field the viewer's `dt_s` is scaled by and the same address its comment already cited. Slot `0x48` is a float inside the interpolated keypoint record at `[esp+0x38]`, and the same routine pins that record's layout down: a `Vec3` at `+0x38`, a packed colour at `+0x44` whose top byte becomes emitter`+0x8c` after `× 1/255` (`0x8eadc0`) at `0x71a337`, the float at `+0x48`, and four floats at `+0x4c…+0x58`. That is exactly the `.gfx` keypoint's `Position`/`Color`/`Scale`/`Direction`, and `+0x48` is separately pushed three times as a uniform vector into `0x787de0` at `0x71a346`, filling the element's own scale at `+0x1b8`. So the scalar is **keypoint `Scale` × root `PlaySpeed`** — taken before the affector update `0x716240` runs at `0x71a37c`, so no controller's scale override can reach the emission volume.

**The unbound basis.** `0x729763` reads its argument three times into `V = scalar × AreaSize` (the `push`/`fstps (%esp)` idiom shifts the frame, so `0x74(%esp)`, `0x7c(%esp)` and `0x80(%esp)` are all `[entry+4]`), builds the quaternion's vector part from emitter`+0x74/0x78/0x7c` at `0x7297ad`, and writes row *i* as `rotate(axis_i, q) × (V_i × 0.5)`. Rows 0 and 1 spell the rotation out inline — `0x4093d0` twice, `0x633700` once with the doubled w from `0x7297f4` and once with `2.0` (`0x40000000`), `0x4071f0` twice with the deliberate leftover argument — and row 2 reaches the same result through the library pair `0x634420` and `0x401ce0`. The three rows are stored at `0x72987c` (`+0xc0`), `0x72994d` (`+0xcc`) and `0x729988` (`+0xd8`), against the axes `0x9c0358`, `0x9c0348` and `0x9c0338` with `V.x`, `V.y` and `V.z`. The bound branch at `0x729684` reads `AreaSize.x` and multiplies by the 0.5 at `0x8d5894` without ever touching the argument, which is why the scalar changes unbound emitters only.

**Where the quaternion comes from.** The unbound arm computes it at `0x71a406`–`0x71a493` and stores it at `0x71a4dc`–`0x71a4f1`: a Hamilton product of the keypoint record's `Direction` with the quaternion `0x6fa810` returns, which is **root`+0x1b4`** when root`+0x1f4` and element`+0x131` are set and either element`+0x132` or root`+0x284` is, otherwise **root`+0x1a4`** — the effect's own placement orientation, which the game fills from whatever owns the effect. The product's order was checked term by term against both expansions: the scalar part matches either, but the vector parts match only `Q ⊗ keypoint`, so the keypoint direction is applied first and the root orientation second. `0x71b450` then recomposes emitter`+0xa0` from it (`0x71a4f4`), and the element caches both the position and the quaternion at `+0x210`/`+0x21c` (`0x71a537`–`0x71a57b`) — the pair the spawn loop slerps from at `0x716003`.

**Why only the scalar was missing.** The viewer's `placement` is `base × rotation_translation(sampled.direction × orbit, …)`, so every particle position it produces is already turned by the interpolated keypoint direction, one stage later than the client turns its basis. The client's unbound particles are world-space: both samplers add emitter`+0x68`, which the unbound arm fills at `0x71a4c4` from the position `0x7872d0` returns, and a world position is not something a later element transform can be applied to. The two compositions therefore agree wherever the viewer's parent basis carries the root quaternion `Q`. What the viewer had no copy of at all was the scalar.

**Shipped.** `ElementContext` carries `emission_scale`, the sampled keypoint scale times the root `PlaySpeed` before the controllers, and `spawn` multiplies it into the half-extents of an unbound emitter of id 121, 123 or 124 — the three classes whose slot [9] is the basis builder. Ids 120 and 122 hold `0x56fbb0` there and never fill a basis; id 125 holds its own `0x727220`, which writes a scalar at `+0xc0` instead of a matrix, and its sampler reads `Radius`, which the factor does not touch. Six tests: `unbound_emission_carries_the_basis_scalar` (a `Scale` of 2 under a `PlaySpeed` of 3 stretches a `(2, 1, 0.5)` volume to `(6, 3, 1.5)` and still reaches its semi-axes), `bound_emission_ignores_the_basis_scalar`, `classes_without_the_basis_builder_ignore_the_scalar`, `the_sphere_radius_ignores_the_basis_scalar`, `surface_emission_carries_the_basis_scalar` (id 124's cylinder cross-section moves out with the factor), and the corpus test below. 78 library tests pass in the offline crate, 74 of them without the archive.

**Measured.** **4,761** unbound elements of ids 121, 123 and 124 author an `AreaSize` — the parser's count, and an independent `GFXELEMENTID`-split scan of the archive text gives the same 4,761. **1,319** of them play under a factor other than 1 on their first keypoint; across all their keypoints, 1,605 of 5,342 scales differ from 1.0, the largest is 10, one is 0 and none is negative. **512** collapse to a zero extent and are skipped rather than divided by, and 1,430 of the 5,459 roots author a `PlaySpeed` other than 1. The sweep's quad count is **unchanged at 34,416,109**, which is the check that matters here: the scalar moves positions only, and a position change that moved populations would mean the sampler's draw order had shifted with it.

**One census note this pass corrected.** Extracting the whole archive rather than only its `.gfx` members shows **1,209 further members carrying the same `MOXTVersion` container header**: 1,071 `.sgc`, 95 `.att`, 40 `.ecm`, 2 with no extension and 1 `.gfx_err`. Their container versions are 2–5 (all 1,071 `.sgc`), 4/11/13 (all 95 `.att`) and 18–58 (the rest), so nearly all of them predate the format gate the `.gfx` population sits above, and only 2 of the 1,209 parse as roots. The corpus is therefore defined as the 5,459 `.gfx` members, which is what every number in this document counts, and the sweep's parsed figure of 5,457 stands (the two known failures are the version-4 `gfx/sgc/刺客_致盲_技能击中.gfx` and the four-byte `gfx/人物/技能/羽芒/翼击击中.gfx`).

## The emission direction, reproduced

*(6 November, fourth pass.)* The direction slot `0x723440` is decoded end to end — see "The direction slot `0x723440`" in the reverse-engineering document — and the player now follows it. Two of its rules the viewer had wrong, and one detail of the cone's construction.

**The axis was Y; the client's is Z.** `0x723447` tests `IsBind` at emitter`+0x85`. A bound emitter gets the global Z axis `0x9c0338` copied verbatim into node`+0x44` (`0x723451`, `0x723503`); an unbound one gets `rotate(Z, quat(emitter+0x74))` (`0x72345b`–`0x7234ff`). `emission_axis` fell back to the element's **Y** after the keypoint orientation, so every element that authored no cone pointed its particles somewhere else — **9,722 of the corpus's 13,675** particle elements author `Angle: 0`, and **1,064** of them also author a nonzero `Speed`, where the axis is the whole of the motion. **3,082** elements author a nonzero `Speed` at all.

Because the player keeps particle positions in the element-local frame that `placement` transforms at draw time (`quad_corners`), the direction has to be stated in the same frame, and the two cases differ. For an unbound emitter the client's `rotate(Z, q)`, with `q = Q ⊗ keypoint Direction` and `Q` already in the parent basis, reduces to the local Z exactly — the orbit ctrl 100 adds is itself a rotation about Z, so it cancels. For a bound emitter the client's Z is bone local while the player's local frame carries the keypoint direction as well, so `emission_axis` undoes that with the inverse spin: **2,770 of the corpus's 9,142** bound particle keypoints (30.3%) author a non-identity `Direction`, so the correction is not hypothetical. Both cases are asserted through `placement`, which is the frame the client states its rule in.

**`ParIniDir` was being read as a direction.** The slot never touches emitter`+0xa0`, where the parser composes `Euler(ParIniDir) ⊗ quat(+0x74)` (`0x7243b8`); those four floats are copied into node`+0x34`, the particle's initial *orientation* — `0x7283f5`–`0x728408` does it inside id 123's volume sampler, right beside the `call *0x4(%edx)` at `0x728413` that fetches the direction. So `ParIniDir` is now parsed and unused, which is what the client's own split implies: the player models particle orientation as the 2D spin in `rotation`, and a quaternion orientation at node`+0x34` would need a billboard basis that reads it. **100** records author a nonzero `ParIniDir` and **9,326** author zero; the old code was wrong for the first group in kind and for the second in axis.

**The cone's construction is now the client's, draw for draw.** `0x72363b` turns the azimuth into an axis-angle quaternion about the emission direction, `0x723661`–`0x7236e9` spin the perpendicular axis by it, `0x723718` turns the polar angle into a quaternion about that spun axis, and `0x723741`–`0x7237c1` tilt the direction by it, copying the result into node`+0x44` at `0x7237d0`. The perpendicular is the global X `0x9c0358` when bound (`0x723556`) and `rotate(X, q)` when unbound (`0x723560`) — a fixed reference axis, where the port took whatever perpendicular `Quat::from_rotation_arc` happened to produce. The distribution was already right (polar uniform in `[0, Angle]` and never clamped, azimuth uniform over the turn, gate `Angle != 0`), so this changes which direction a given pair of draws yields rather than where the samples fall.

**Shipped.** `ElementContext` carries `spin` — the rotation `placement` adds on top of the parent basis — and `keypoint_direction`; `emission_axis` returns the direction together with its perpendicular; `cone_direction` takes both and leaves the geometry to `cone_sample`. Four tests: `the_emission_axis_is_the_element_forward_axis` (local Z, and the world direction is the keypoint direction turned Z), `bound_emission_unwinds_the_keypoint_direction` (asserted in the bone frame, where the client states it), `par_initial_direction_is_not_an_emission_axis` (the corpus's own degree conversions leave the axis alone) and `the_cone_tilts_from_the_spun_perpendicular` (a quarter turn with no azimuth lands on −Y, which is `rotate(Z, axis_angle(X, π/2))`). The two existing cone distribution tests pass unchanged, which is the point: the samples fall where they did, they are just labelled by the client's reference axis.

**Measured.** The whole-corpus sweep is unchanged at **34,416,109 quads and 0 non-finite**, which is the check that a direction change moves trajectories and nothing else, and the two surface corpus tests still report their 1,520 id-123 and 1,045 id-124 elements. 82 library tests pass in the offline crate.

## Parity census

The player reads 137 of the 205 fields `gfx_file` exposes; the remaining 68 are parsed, kept and enumerated in the `gfx_file` audit output rather than dropped, so a later pass can pick them up without re-reading the grammar. Measured over the corpus:

| Group | Measurement |
| --- | --- |
| Roots | 5,459 members, 5,457 parse (one empty container, one version-4 root that predates the client's own gate of 24), 31,019 elements |
| Particle elements | 13,665: id 120 1,315, 121 320, 122 40, 123 9,869, 124 1,739, 125 382 |
| `Angle` | 9,719 zero, 2,130 at 2π, 416 at 30°, 256 at 3°, 248 at π, 131 at 20°, 36 negative; range ±6.283185 |
| `Speed` | min −100, max 150, 10,590 zero, 340 negative |
| `EmissionRate` | max 20,000 |
| `TTL` | max 1e6 |
| `AreaSize` | 912 distinct triples, only on ids 121/123/124 (11,928 elements) |
| `Radius` | 15 distinct values, only on id 125 (382 elements) |
| Emission basis scalar | keypoint `Scale` × root `PlaySpeed`, read only by unbound ids 121/123/124: 4,761 such elements author an `AreaSize`, 1,319 play under a factor other than 1 on their first keypoint, 1,605 of their 5,342 keypoint scales differ from 1.0 (max 10, one 0, none negative), 512 collapse to a zero extent; 1,430 of 5,459 roots author a `PlaySpeed` other than 1 |
| `DragPow` | 9,854 records, max 10; `IsDrag` on 242 |
| `ParIniDir` | 9,326 zeros, ~100 exact degree conversions |
| `IsSurface` | 2,674 (id 123: 1,520, id 124: 1,045, id 120: 86, id 121: 11, id 125: 12 inert); `IsBind` 8,259 (id 123 alone: 5,682) |
| `IsUseHSVInterp` | 168 elements in 101 roots; 147 with distinct endpoints, 144 of those differing in hue and sweeping 0.567 of the circle on average, 4 taking the wrap branch, 52 fully saturated at both ends |
| `IsAvgGen` | id 123: 45, id 125: 373; `AlphaSeg` only on id 123 (6,641, default 10 in 6,604) |
| `Priority` | ~30,000 records, values 1…4 |
| `ScaleNoOff` | 249; `IsUseParUV` 10 |
| Root keys | `UseAABB` 3,490, `ShakeCam` 241, `Raytrace` 109, `NoChangeDir` 81, `2DBackLayer` 12, `2DRender` 3, `PhysExist` 0 |
| Children | 1,530 `GfxPath` references, 1,448 resolve inside the archive, 82 do not (31 distinct authoring mistakes), 484 distinct child roots loaded by the sweep |

## Verification

The three corrections were compiled and tested against a vendored glam 0.27 with `encoding_rs` replaced by a byte-preserving latin-1 stub, which is the only offline configuration available here; `src/modules/**` and `gfx_effect_gpu.rs` are compiled by CI alone. 42 library tests pass, five of them new (the ellipsoid, the ball volume, the polar-angle distribution, the unclamped cone and the truncating colour sample). One test fails in the offline crate only — `gfx_file::tests::gbk_decode_reports_damage` asserts real GBK behaviour that the stub cannot provide — and passes in CI.

The whole-corpus sweep was re-run after the changes: every root parsed, a player built per root with children resolved against the corpus itself, 100 frames of 100 ms advanced each, then frame zero compared against `gfx_decal`, the independently verified static reader, for every root that reader accepts.

```
frames 100 x 100 ms / files 5459 / parsed 5457 / played 5457 / child-roots 484 /
quads 19861347 / max-per-frame 4079 / max-particles 4257 / finished 2658 /
warnings 84 / cycles 0 / PARSE 2 / BUILD 0 / ADVANCE 0
decal equivalence: roots 13, quads 19, mismatches 0
```

The two parse failures are the known pair (an empty container, `gfx/人物/技能/羽芒/翼击击中.gfx`, and the version-4 `gfx/sgc/刺客_致盲_技能击中.gfx`). All 84 warnings are unresolved `GfxPath` children — retail files that do not ship — and no root warns about a cycle or a nesting depth. The earlier key census counted 82 such references over 5,458 files; this sweep covers 5,459, because it recurses rather than globbing `*.gfx` and so picks up the one member named exactly `.gfx`. The decal equivalence line is the one that matters for these changes: the sprite path the repository already verified against the client's decal calls is byte-identical to the player's frame zero after the emission rewrite, so the corrections touched particles only.

The sweep harness and the corpus live outside the repository (the archive is a client asset and is not committed); the harness configuration above is what makes its numbers reproducible.

## Fixed while writing this up

`gfx_light` built its controller-108 `ColorTrack` from the file's raw `StartTime`/`EndTime`, which are milliseconds, while a track's window is in seconds (`gfx_color_track`: "Start/end are seconds; segment lengths are milliseconds") and `Emitter::tick` samples it with the seconds elapsed since the element's own `StartTime`. A controller starting at 500 ms therefore never opened — the light held its keypoint colour for its whole life — and a window that ended never expired. Every existing test authored `StartTime: 0` with `EndTime: -1`, where the two units coincide, which is why it stayed latent. The window is now converted at construction and the segment lengths stay in milliseconds, as the track's table walk expects; `the_color_ramp_window_is_converted_from_milliseconds` pins a non-zero window against the untouched keypoint colour.

The same reading opened a question this pass does **not** settle. `ColorTrack::sample` gates on its own `start`/`end` and then walks the 128-entry tables from zero of the same argument, so one value carries both the gate and the walk origin. `gfx_light` passes unshifted seconds since the element's `StartTime`; `gfx_player` passes `age − control.start / 1000` for controllers 108 and 111, which moves the gate as well as the walk, so a controller starting at 500 ms would open at 1,000 ms of element time. Controllers 100 and 101 were read out of the binary as skipping while `age < StartTime` yet integrating the *absolute* `age`, which favours `gfx_light`'s unshifted form, but the controller-108 update routine was not re-read this pass and neither call site was changed on a guess; both carry a comment saying so. Settling it needs the executable again.

## The HSV colour path, reproduced

`IsUseHSVInterp` was the one behaviour in the list above that the binary could settle on its own, and it now is: the viewer runs the client's second colour sampler for the 168 retail elements that ask for it.

**The branch.** `0x7237f0` loads the two packed colours from emitter`+0x30`/`+0x34`, returns the minimum untouched when they are equal (`0x7237fc`), and otherwise tests the byte at emitter`+0xb0` (`0x723804`): zero goes to the RGB sampler at `0x7238c3`, nonzero to `0x723812`. The two are not variants of one idea.

- RGB (`0x7238c3`) calls `rand` four times (`0x7238cc`, `0x7238e0`, `0x7238f4`, `0x723908`), scales each by 1/32767 (`0x8e6f10`) and combines one per channel as `min * (1 - u) + max * u`, truncating through `_ftol` — the thunk `0x892e78` resolves to `MSVCRT._ftol`, which pops, and that pop is what fixes the stack accounting for both converters. The alpha is then multiplied by emitter`+0x8c` at `0x723955` and truncated a second time.
- HSV (`0x723812`) calls `rand` **once** and uses that single `u` for all four channels, lerping the other way round, `min * u + max * (1 - u)`. For a uniform draw that is the same per-channel distribution, but it couples the channels: a particle cannot come out bright and pale by coincidence the way the RGB path allows.

**Hue wraps.** `0x723829` compares the two hues and, when the minimum is the larger, `0x72383b` adds a full turn to the maximum (`0x72383e`) and `0x72385e` takes it back off any result above one, tested at `0x723851`. An author writing 0.9 down to 0.1 gets the short way round through red rather than the long way back through the spectrum. The four lerped values are stored as singles (`0x723876`, `0x723890`, `0x7238a0`, `0x7238ae`) into a four-float struct that `0x7898f0` converts back; the packed result is read at `0x7238b7`.

**`0x7898f0`, HSV→RGB**, cdecl `(const float hsva[4], DWORD* out)`. The out pointer is read at `0x789aab`, after the `pop %edi` at `0x789aa5` has shifted the frame — which is why the same slot offset serves as the value channel's local earlier and as the out pointer at the end. Alpha is `trunc(a * 255)` (`0x789916`, scale constant `0x8f0ae8` = 255.0). Value is `v * 255 + 0.5` (`0x789906` then the bias `0x8eff98` = 0.5 at `0x78990c`) before `_ftol`, so **value rounds to nearest while alpha truncates**. A zero saturation, compared against `0x8effc4` = 0.0 at `0x789924`, short-circuits to grey at `0x789933`. A hue of exactly 1.0 is folded to 0.0 (`0x789947` compares against `0x8effa0` = 1.0, `0x789956` loads the zero) so the top of the circle lands in sector 0 instead of off the end of the switch. Otherwise `h * 6` (`0x8f0d80` = 6.0) is floored through `MSVCRT.floor` at the IAT slot `0x8d54b4` and truncated to the sector at `0x789975`; the fraction feeds the three textbook products `p = (1-S)V`, `q = (1-Sf)V`, `t = (1-S(1-f))V` at `0x789997`, `0x7899ab` and `0x7899c5`. The switch at `0x7899d1` runs through the jump table `0x789ab8`, and its six cases are exactly the hexcone: (V,t,p), (q,V,p), (p,V,t), (p,q,V), (t,p,V), (V,p,q). `cmp $5,%eax; ja 0x789a8f` sends a seventh sector to a path that reads the value slot as an integer; hue is normalised into [0, 1], so it cannot be reached, and the port clamps to the sixth case rather than copying dead code. Packing is `(A<<24)|(R<<16)|(G<<8)|B` at `0x789a9b`.

**`0x789ad0`, RGB→HSV**, cdecl `(DWORD packed, float out[4])` — confirmed by the single `add $0x10,%esp` at `0x724445` cleaning up both calls' arguments at once. It unpacks R/G/B/A at `0x789ae1`, `0x789af4`, `0x789afd` and `0x789b0d`, each scaled by the f32 1/255 at `0x8f0928`, and copies the alpha through as that float at `0x789be8` rather than converting it back. Value is the channel maximum (`0x789bf3`); the maximum is tested against zero at `0x789bf6` before dividing, so black stores a zero saturation instead of a quotient, and the saturation is tested again at `0x789c15` so an achromatic colour stores a literal zero hue at `0x789c29`. Hue comes from the three-way maximum test (`0x789c33` red, `0x789c52` green, `0x789c77` blue) with the sector offsets +2 (`0x8f0d90`) and +4 (`0x8f0af8`), scaled to degrees by 60.0 (`0x8f0d8c`) at `0x789c98`, wrapped by +360 (`0x8f0d88`) at `0x789cab` when negative, and normalised by 1/360 (`0x8f0d84`) at `0x789cb1`.

**The parse-time conversion is unconditional.** `0x724434` converts `ColorMin` into emitter`+0x38` and `0x724440` converts `ColorMax` into `+0x48`, from the key-parse epilogue at `0x724428`, whether or not the flag is set; only the choice of sampler depends on it. The flag itself is read as *not zero* rather than *equal to one*: `0x724414` scans the value, `0x72441d` tests it against itself and `0x724422` stores the `setne` result in emitter`+0xb0`. `gfx_file` now reads that key the same way, through a `nonzero_flag` reader — the only flag in the grammar with disassembly behind a wider reading than 0/1, so the flags beside it stay strict.

**What it changes for retail content.** 168 elements in 101 roots. 21 author equal endpoints and short-circuit before the flag matters. Of the other 147, 144 differ in hue and sweep 0.567 of the circle on average, 4 take the wrap branch, and 52 have both endpoints fully saturated — the case where the RGB path's independent draws put grey midtones between two pure hues and the HSV path cannot.

**Verification.** Nine new tests: the converter against hand-checked primaries, greys, black and a half alpha (`rgb_to_hsva_matches_the_client_converter`); a 16,384-colour round trip through both routines (`hsv_conversion_round_trips_the_palette`), which is exact and is the strongest available check that the two agree on layout, sectors and bias; the rounding bias against the truncating alpha (`the_value_channel_rounds_and_the_alpha_truncates`); the grey short-circuit ignoring hue (`zero_saturation_is_grey_whatever_the_hue`); the hue fold (`a_hue_of_exactly_one_folds_into_sector_zero`); the draw counts (`the_hsv_path_shares_one_draw_between_its_channels`); the saturation consequence (`the_hsv_path_holds_saturation_across_a_hue_sweep`); the wrap (`hue_wraps_the_short_way_when_the_minimum_is_above_the_maximum`); and the flag reading (`the_hsv_flag_is_a_nonzero_test_not_an_equality_test`). 72 library tests pass in the offline crate, which now vendors the real `encoding_rs` 0.8.35 from GitHub alongside `cfg-if` 1.0.5 instead of the latin-1 stub used earlier, so `gbk_decode_reports_damage` passes locally as well as in CI.

The corpus sweep was re-run afterwards. Every line is unchanged — 5,457 of 5,459 roots parsed and played, 484 child roots resolved against the corpus, 84 warnings over 31 distinct missing files, PARSE 2 / BUILD 0 / ADVANCE 0 / cycles 0, decal equivalence 13 roots / 19 quads / 0 mismatches — except the quad total, which moved from 19,861,347 to 19,861,420. That is the sampler's draw count rather than its colour: one draw per spawn instead of four shifts every later value those emitters pull, so a handful of particles land differently across 545,700 frames. Max per frame (4,079), max particles (4,257) and the finished count (2,658) are identical.

## Camera shake, reproduced

`ShakeCam` is set on 241 roots and was the last root-level behaviour the viewer ignored. It is decoded far enough to run: `gfx_player::Player::camera_shake` returns the offset the client would hand its camera on the frame `advance` just produced.

**The parse.** `0x6e38a4` gates the key on version 24, `0x6e38ce` scans it with `sscanf` against the string `ShakeCam: %d` at `0x8eb790`, `0x6e38d9` tests the value against itself and `0x6e38de` stores the `setne` result in a single byte at root`+0x1f6`. There is no bitfield: the client keeps one bool. The corpus agrees — 5,216 roots author `0`, 241 author `1`, and nothing authors any other value. `gfx_file` had been reading bit 0 as "shake the camera position" and bit 1 as "shake its scale"; both accessors are gone, replaced by `shakes_camera()`, which is the client's own nonzero test. Neither accessor had a caller anywhere in the tree, so nothing else moved and no parse or playback path changed.

**A root controller is an element controller.** When the flag is nonzero, `0x6e38ea` calls `0x6eae10`, which reads `CtrlType: %d` (`0x8ebafc`) and, from version 21 on, `StartTime: %f` (`0x8ebb0c`) and `EndTime: %f` (`0x8ebb1c`); earlier versions default them to 0 and −1.0 at `0x6eaef8`. The type goes to `0x6ea910`, which subtracts 100, range-checks against 12 and jumps through the table at `0x6eaddc` — the same thirteen-case controller space the elements use. That factory has exactly two callers, `0x6eaf0d` here and `0x71b619` in the element path, which is direct evidence for the choice `gfx_file` already made: one `Control` grammar, parsed at root level and element level alike. The constructed object loads itself through its vtable at `0x6eaf2b`, keeps `StartTime` at `+0x8` and `EndTime` at `+0xc` (`0x6eaf4c`, `0x6eaf53`), and is stored on the root at `+0x208`.

**The shake.** One caller, `0x6e4b7d`, runs `0x6e4dc0` once per frame:

- `0x6e4dc9` requires root`+0x1f6` and `0x6e4ddb` requires the controller at `+0x208`.
- The window uses the root's millisecond tick at `+0x1cc` scaled by 0.001 (`0x8d5c70`) at `0x6e4df5`, so the controller's start and end are in seconds. A negative `EndTime` skips the end test altogether (`0x6e4e0c` compares against 0.0 at `0x8d58a0`) — the same "forever" convention the rest of the format uses.
- On the first frame `0x6e4ffb` latches `+0x1f7`, `0x6e5002` zeroes a frame counter at `+0x1f8`, and `0x6e5008` calls `0x41a480` once: the camera is saved before it is disturbed.
- Every frame increments `+0x1f8`. **Odd** frames take `0x6e50f3`, which negates the three floats at root`+0x1fc..+0x204` and re-applies them through `0x41a4e0`. The shake alternates sign at half the frame rate instead of drawing a fresh sample every frame.
- **Even** frames compare root`+0x29c` against 30.0 (`0x8eb710`) at `0x6e5037`. At or beyond 30 the offset is zeroed; inside it, `0x6e5094` forms `1.0 − root+0x29c/30.0`, `0x6e50a5` calls the noise generator `0x766490` with `this = controller + 0x14`, and the result is scaled by that factor and by a fresh random unit direction before it is stored into `+0x1fc` and applied.
- The direction comes from two `rand()` draws, each multiplied by 2π/32767 (`0x8e6f14`), turned into quaternions by `0x7b6fd0` about the globals `0x9c0348` and `0x9c0358`, and composed (`0x6e4e55`, `0x6e4f25`). Both globals live past the raw image, so their bytes are not in the file — but two static-initialiser thunks write them: `0x6ea170` stores (0, 1, 0) at `0x9c0348` and `0x6ea140` stores (1, 0, 0) at `0x9c0358`, so the two rotations are about the unit Y and unit X axes. Which one the client applies first is not readable out of the quaternion helper sequence, and the two draws are made every frame inside the window whether or not the frame uses them.
- root`+0x29c` is the same field the emission LOD divides the particle count by, so one distance input drives both the emission rate and the shake amplitude.

**The generator.** `0x766490` is a gradient noise over a table the object owns: `this+0x18` is a count that abandons the whole call when it is not positive (`0x7664a6`, `0x7664af`), `this+0xdc` is a modulus, and `this+0xe0` is an array of twelve-byte records. Each step splits a coordinate into an integer index and a fraction — `_ftol` at `0x7664e6`, then `fsubr` at `0x7664fa` — wraps the index into range by adding the modulus while it is negative (`0x766504`) and taking a signed remainder (`cltd`/`idiv` at `0x766508`), copies the gradient record for that index and for `index+1` with `rep movsl` and a component count clamped to three (`0x76650b`–`0x766564`), and interpolates: a byte argument picks between two variants at `0x766574` and the blend starts from 1.0 (`0x8effa0`) at `0x76657c`.

**Reproduced.** The three inputs that had kept it out turned out to be available. The distance root`+0x29c` is the same quantity `distance_fade` already measures — camera to the root's origin — so the shake reads it from the `View` the caller passes. The rotation axes are the two initialiser thunks above. And the save-and-apply pair does not have to be reproduced at all: the player owns no camera, so `camera_shake` returns the offset at root`+0x1fc..+0x204` and leaves the applying to whoever holds the camera. `CharacterGfx::camera_shake` sums the roots an effect list is playing, since the client applies each root's offset in turn, and `Walker::camera_shake` exposes that to the host that builds the frame's view-projection.

The window, the latch and the parity are literal: `StartTime` and `EndTime` in seconds against the root's millisecond tick, a negative `EndTime` skipping the end test, the counter zeroed when the latch is set rather than when it is cleared, the first frame in the window an even one, and the odd frames negating the stored offset in place. Three places where the viewer deliberately reads differently:

* A nonzero `ShakeCam` whose `CtrlType` is outside 100..=112 leaves `0x6eae10` returning null, which fails the whole root load at `0x6e38f8`. The viewer reports no shake instead of rejecting a file, the way it warns about an unresolvable child reference rather than dropping the root that holds it. No corpus file hits the case: all 241 author 112.
* The composition order of the two rotations is not readable, so `shake_direction` returns a fresh random unit direction built from the same two angles. Both readings span the sphere; they are not the same sequence.
* The generator's fade curve and octave weighting are still undecoded — the shape is identified at `0x766490`, the interpolation is not — so the sample comes from the viewer's own `fbm`, the normalised hash-noise ladder element 150 uses. One difference matters: element 150 redraws its lattice seed at every regeneration, while the client's noise object builds its gradient table once, in its constructor `0x7662b0`, and the driver only samples it. The shake therefore keeps a per-player seed for the life of the effect, which is what makes consecutive frames correlated rather than white noise.

The amplitude is authored on the controller and applied by the noise object, not by the driver, so the viewer multiplies the sample by it — the same place element 150's amplitude acts. Measured against the corpus, all 241 shaking roots leave `BufLen`, `WaveLen`, `Persistence` and `OctaveNum` at the constructor's 256, 1, 0.25 and 5, and vary only `Amplitude`: 1.0 on 89 roots, 0.5 on 35, 0.1 on 32, 0.2 on 24, 0.3 on 20, 0.05 on 9, 2.0 on 8, and one each at 5.0, 3.0 and 2.5. Seven author 0, which disables a shake the file still asks for.

The census test plays all 241 and 232 of them move the camera within two seconds. The nine that do not are accounted for: five author `StartTime: 0` with `EndTime: 0`, a window one instant wide that no 16 ms frame lands inside; one authors `StartTime: 3`, past the two seconds the sweep plays; and seven author `Amplitude: 0`, four of them overlapping the first two groups. Six plus seven minus four is nine, and 241 minus nine is 232.

## Placed in the viewer on 6 November

Two recovered behaviours moved from the documents into the executable, and one
interface was added so the rest can follow.

- **Every effect is now addressable.** `src/gfx_catalog.rs` reads the archive
  directory with the retail v2 reader the crate already ships, groups all 5,459
  `.gfx` members by the names their authors gave them (water 233, fire 259, sun
  rays 30, glows 59, lightning 123, smoke 219, weapon trails 986, monsters
  1,540, characters 1,405, scene dressing 209, interface 10, other 386 — every
  member in exactly one group), reports what a root contains before anything
  spends a load on it (element counts, particle emitters, lights, `IsSurface`
  and `IsUseHSVInterp` emitters, `ShakeCam`, the largest `Priority`, nested
  children), says which of the two readers will play it, and hands back a
  `gfx_owner::Request`. It is built where the archive is attached
  (`world_models_gpu::attach_gfx_archive`) and exposed as `effect_catalog`,
  `effect_census` and `spawn_catalog_effect`. Seven curated presets — sun rays
  through trees, light pillars, moving fire and lava, waterfall spray, glows and
  halos, lightning, weapon trails — list 28 real archive paths, each checked to
  exist and to classify under the group it is listed in.
- **A 3D particle's orientation is in one frame now.** `particle_basis` mixed
  `state.direction`, which is element-local (the emission samplers produce it
  there and the integrator moves `state.position`, which `quad_corners` sends
  through `context.placement`), with `View`'s axes, which are world. The result
  depended on the element's placement rotation twice. `oriented_basis` brings
  the direction into the world frame with the same placement the billboard path
  uses for its centre; an identity placement is bit-for-bit unchanged.
- **The catalogue is on screen.** `src/modules/walker/gallery.rs` plays any root
  the catalogue names, standing in the world with no character attached: it opens
  `element/gfx.pck` through the Walker's own `Archive`, builds a
  `gfx_player::Player` per root (nested `GfxPath` children resolved against the
  same archive, missing children kept as warnings), advances them against the
  frame's recovered camera basis and appends their quads to the same
  `gfx_effect_gpu::Draw` the character effects use, so blend buckets and texture
  resolution are unchanged. The Walker's settings panel gains an "Effect gallery"
  section: a preset selector covering all seven curated presets, a row placement
  150 units in front of the eye at 80-unit spacing along an axis flat and
  perpendicular to the look direction, a clear button, and a search box over the
  5,459 catalogue members that lists up to 40 hits as spawn buttons. Gallery roots
  go straight to `gfx_file` + `gfx_player` and are never offered to `gfx_decal`
  first: that split exists to keep long-verified static roots on their old path,
  and here the full grammar is the path the corpus tests cover.
- **Both level-of-detail rules are now written down as code**, not only as
  prose: `gfx_catalog::priority_exempts` for `0x70ba00` and
  `gfx_catalog::detail_scaled_count` for `0x715b88`..`0x715bfd`, with the
  thresholds 10.0 and 40.0 and the `0x9c1318` factor 8.3333333 that
  `0x715150` computes. The emission rule is tested but deliberately not wired:
  nothing in `0x700000`..`0x730000` writes root+0x298, root+0x29c or the
  root+0x2a0 enable byte, so the numerator has no recovered provenance, and
  inventing one would put a guess in the emission path.

## Placed in the viewer on 7 November

Six recovered behaviours moved from these pages into `gfx_player` and the walker,
in the order the corpus said they mattered:

- **Binding by name.** A root's elements are indexed by `Name` once, after every
  element exists so a `BindEle` may point forward, and the bound element's
  transform feeds the child's placement each frame. 967 of the corpus's 968
  authored names resolve to a sibling; the one that does not stays unbound and is
  reported, which is what the client does with a name it cannot find.
- **Ground matching.** `advance_with_ground` takes an `Option<GroundHit>` — height
  and normal at the placement the effect is parented to — and builds the ground
  frame `0x6fa7c0` selects and the height frame `GroundHeight` selects from it.
  `walker/physics.rs::effect_ground` supplies the sample from the walker's own map,
  one query per effect per frame, so 393 of the 459 roots that ask now change
  geometry where before none did.
- **`GrndNormOnly`.** The byte at element+0x150 makes the ground frame carry the
  normal only, without the snapped height, which is the difference between an
  effect lying on the slope and one lying at the slope's altitude.
- **The trail's vertex ring.** Element 152 keeps up to 256 laid vertices, ages
  them by `VertsLife`, drops them from the old end and — when `isappend` is clear —
  rebuilds the whole strip instead, which is why the 18 corpus trails that author
  `isappend: 0` draw their complete strip from frame one.
- **`ResetLoopEnd` for trails.** See the corrected subsection above: the flag
  empties the ring at the loop boundary, and the flipbook reading the base class
  makes of it is documented beside the code rather than implemented.
- **Camera shake.** See *Camera shake, reproduced*.
- **The world's own effects draw where the map puts them.** An installed effect
  owner produced lights and nothing else — `gfx_owner`'s header listed the rest of
  the GFX graph as explicitly not implemented — so the 2,275 placements
  `world.ecwld` authors, 163 distinct roots among them (154 day-only, 676
  night-only, 1,445 all-day by `valid_time`), lit their surroundings and showed no
  water, fire, mist or glow. `Manager` now builds a `gfx_player::Player` per owner
  from the same archive text its light emitters come from: one root per frame, the
  budget `0x5b1a00` gives the loader, and only while the owner is inside the host's
  activation radius, so a world full of effects costs the handful near the
  explorer. `Manager::effect_quads` advances those players at the placement, play
  speed, authored alpha and parent attachment the owner already carried, and
  `world_models_gpu::World` submits them through a `gfx_effect_gpu::Draw` of its
  own — the same reverse-Z depth as its models, after the alpha meshes, in the pass
  it already draws into. The effects therefore belong to the world and to nothing
  else: no add-on module, no explorer body, no gallery row. `ViewSettings::world_effects`,
  in the World tab and on by default, gates the geometry alone: off releases every
  player and keeps every light, so a torch stops drawing its flame and still lights
  the wall, and on rebuilds one root per frame instead of stalling on all of them.

  Three places where this is the viewer's budget rather than the client's rule, all
  recorded where the code can be checked against them: an owner advances only while
  it is visible, where `0x70ba00` advances the clock *before* the visibility gate
  and so keeps an off-screen effect running; an owner that leaves the radius drops
  its player and starts again when it returns, which is what a region unload does
  but not what a radius crossing does; and nothing at this level can answer a ground
  query, so `MatchGrnd` and `GroundHeight` stay inert for world effects. The
  authored attenuation curve is still not applied either — `Request::outer`'s alpha
  scales the root, the distance fade the visual-effect record carries does not.

  A nested `GfxPath` the archive does not ship is a warning with the consequence in
  it ("the root plays without this child") rather than a bare reader error: 82 of
  the corpus's 1,530 child references are like that,
  `gfx/人物/技能/巫师/技能抑制_闪电.gfx` asking for a `牧师` effect the archive has no
  directory for among them, and the client does not fail a parent over a child it
  cannot open.

- Seeing it: the World tab carries "World effects (waterfalls, torch fire,
  glows)", on by default, and its status line reports how many roots are
  playing, how many quads they drew and how many roots would not build.
  Unchecking it releases every animated player and keeps every light, so the
  same scene shows the cost of the geometry alone; checking it again rebuilds one
  root per frame rather than all of them at once. The Sound tab's separate
  "Play world effect sounds" switch has always been about audio, and is renamed
  to say so now that the two sit in one settings window.

### The effect record, re-read against the shipped map (correction, 7 November)

The world effects first went in with the ECWLD effect record's fields four
bytes late from the scale onwards, and the first in-app look showed why that
matters more than a misparse usually does: an emission-driven root whose clock
never moves draws *nothing*, so the map looked like it had no effects at all —
the static white column at a fall is the scenery texture underneath, and the
day-only shafts and glows were absent rather than wrong. A second, partial
correction moved the reader to start at the scale, which put the *position* on
the record's first three floats. That is still wrong, and the same symptom
followed it.

**The record opens with a payload, not an id, and the payload starts at 0.**
The shipped `world.ecwld` decides this on all 2,275 placements at once, and it
decides cleanly:

| reading | unit `direction` and `up`, perpendicular | play speeds | `valid_time` values |
|---|---|---|---|
| record[0] = id, payload from 4 | 2,275 / 2,275 | 0.03 .. 12, none ≤ 0 | {0: 154, 1: 676, 2: 1,445} |
| payload from 0 | 53 / 2,275 | mixed with float bit patterns | 19 distinct values |

The layout the map agrees with is: `[0..12)` position, `[12]` uniform scale,
`[16..28)` direction, `[28..40)` up, `[40]` play speed, `[44]` valid-time word,
`[48]` alpha, `[52]` attenuation as a byte, and `[53..60)` padding that is never
copied. The copy block at `0x5b3436..0x5b34a6` carries the same order — the
position triple first (with the caller's x/z addends), then the two vectors,
then the three dwords and the byte — which is the independent confirmation that
the corpus reading is not a coincidence of this one map.

What the earlier readings cost, in the world: a day-only placement read its play
speed out of its day/night word, which is a float bit pattern of about 1.06e9,
and its position out of the middle of its direction vector. The waterfall spray
a player sees at (2,301, 218, -3,168) came out at (0, -3,605, 292) with a clock
that jumped and froze. Under the corrected reading the same placements sit in
the low thousands on both horizontal axes, at hillside heights, with speeds
0.03..12 and the authored day/night split.

`corpus_effect_records_agree_on_one_layout` holds the shipped map to all of it,
and `corpus_waterfall_plays_from_its_authored_placement` plays a real placement
through the owner manager and requires the owner to go live, build its player,
grow its quad count and scroll its texture inside the authored footprint.

**The x/z addend is zero.** The two `fadds` at `0x5b343a` and `0x5b3449` fold a
caller-supplied pair of floats into the placement's x and z, and the only routine
that supplies them (`0x41bb30`) pushes two `0` immediates before calling the
loader — its own only caller passes no origin either. So effect positions are
absolute world coordinates, the addend needs no recovered block term, and a
placement that looks a whole block off is a different bug.

The play-speed word is stored, not validated: the first world load after the
first correction refused to start over block (18,18)'s first effect, whose speed
word is not a playable rate, and the loader never checks it — `0x5b3700` copies
the float into the root and the tick multiplies by whatever is there, so an
unplayable rate leaves the owner's clock short of its StartTime and retail simply
shows nothing. The viewer now agrees with that outcome instead of arguing: an
unplayable rate freezes the owner at clock zero and lands one line in the effect
notices, and no single placement — scene effect or model event — can refuse the
world load any more; each rejected record is a reported absence on the status
line while the map and its other effects come up. With the record read correctly
none of the 2,275 shipped placements is in that state.

### The effect camera came back out of the frame matrix wrong (7 November)

The effect pass needs a camera basis and `Frame` carries only a
view-projection, so the basis was derived from the inverse. Both halves of that
recovery were wrong, and neither shows up until an effect has to face the
camera or be sized against the eye.

`Mat4::transform_point3` assumes w = 1 and drops the perspective divide, so
asking it for the NDC origin returns the near plane rather than the eye:
measured against this viewer's own projection, 3,911 units away for a camera
looking across the world, and worse the further the camera sits from the origin.
The inverse's columns are not the camera basis either, because this viewer
builds its projection with a clip-space mirror (`from_scale(-1, 1, 1)`, the
handedness flip) and that mirror lives in the projection: the recovered forward
was 0.91 away from the true view direction, with the right and up axes off by
tens of degrees.

The basis now comes from points, which handles a mirrored, orthographic or
skewed projection with one arithmetic: two points on the centre ray give the
view direction, two more give the screen axes, and the eye is where that ray
crosses the plane the inverse sends to infinity — row 3 of the matrix. Measured
afterwards, the eye is within 0.003 units and the direction within 1e-5 on the
mirrored projection, on a plain one, and looking down. Both callers (the world
and the gallery) recover the basis the same way, so this was one defect in two
places. `effect_camera_comes_back_out_of_the_frame_matrix` pins it.

### The world's effects no longer depend on the map placing a model

The effects lived inside the branch that builds the animated models, behind a
check that the map placed at least one EC model. A block that authors
waterfalls, torch fire or a glow and no animated model lost all of it, and so
did any map whose model archive would not open. The two halves share nothing:
every effect root comes from `gfx.pck` and every placement from the map. The
object is now built unless the opt-out says otherwise, and a model archive that
will not open costs the model queue alone (`World::without_models`) instead of
taking the world's effects down with it.

Modules read the world's effect catalogue now and play nothing through it
yet: the host hands `Modules` a clone of the `Catalog` when the archive
attaches, `Modules::effect_catalog` returns it, and the test
`add_ons_can_read_the_world_effect_catalog` pins the lookup path (find by
path, group by family, search by name). Spawning stays
`World::spawn_catalog_effect`, called by the host, until a module has a
reason to ask.

## Running a module's tests without the crate's dependencies

Four CI cycles went into guessing at a wrong assertion when the file did not
build. Two properties of this workspace combine badly: `cargo` cannot run at all
without crates.io, and `cargo check --locked --bins` does not compile
`#[cfg(test)]` code, so an error inside a test module is invisible until the test
step — and the test step's log sits on a storage host this workspace cannot
reach, so the failing assertion is never named.

For a module whose logic needs nothing but `std`, the tests can be run anyway.
`gfx_catalog`'s `normalize`, `group_of`, `Catalog::from_directory`,
`priority_exempts`, `detail_scaled_count` and `presets` are all dependency-free;
lifting them and their tests into one file and building it with
`rustc --edition 2021 --test` runs them in under a second, given a sysroot
assembled from the `@rustbin/rustc-1.88.0-x86_64-unknown-linux-gnu`,
`@rustbin/rust-std-1.88.0-x86_64-unknown-linux-gnu` and
`@rustbin/rustfmt-1.88.0-x86_64-unknown-linux-gnu` npm packages (rustc's
`lib/*.so` into `<sysroot>/lib`, the std rlibs into
`<sysroot>/lib/rustlib/x86_64-unknown-linux-gnu/lib`). That run found what the
cycles could not: converting an `assert!` to `assert_eq!` had orphaned a
positional `{:?}` in the format string, which is a compile error, not a wrong
value. It also corrected a hand-computed expectation in the other direction —
`6.0 * 8.3333333` rounds to exactly `50.0` in f32, so the emission-detail factor
at distance 100 is 0.5 and ten particles become five, not the four a by-hand
truncation argument claimed.

*(Superseded on 7 November: `tools/offline_harness.sh` now builds `glam` and a
generated GBK decoder, stubs `flate2` behind `crate::pck`, and compiles the
twelve dependency-free modules — including `gfx_file`, `gfx_decal`, `gfx_owner`
and `view_settings` — so those tests run locally in about twenty seconds and the
corpus tests skip cleanly when `google_drive/x/` is absent. Only the GPU side
still needs CI.)*

One more CI-only trap this work ran into, recorded because the error points
somewhere that is not wrong. `src/bin/` holds standalone check binaries that
include the runtime modules by `#[path]`, so each has its own crate root and its
own `mod` list. When `gfx_owner` began importing `gfx_player` and
`world_models_gpu` began importing `gfx_effect_gpu`,
`cargo check --locked --bins` failed with *unresolved import `crate::gfx_player`*
at `src/bin/../gfx_owner.rs:39` — a line that is correct in the real crate and
unresolvable only in the one binary that includes the module without declaring
its neighbour. The fix is two `#[path]` declarations in
`model-draw-gpu-check.rs`. The check that keeps it from costing a second cycle is
to walk the transitive closure of `crate::` module references in the non-test
code of every module a bin includes, and diff that against the bin's `mod` list,
instead of adding only the two names the compiler printed first.

## The field census: what is parsed, what is played, and what the version gates allow

Measured over all 5,459 roots on 6 November, by scanning every `Key:` line in
the corpus and then asking, for each field the parsers produce, whether anything
outside `gfx_file.rs` reads it. This replaces guesswork about "what is left" with
a count.

**The grammar is complete.** The corpus uses 200 distinct keys. Nine never appear
in a parser — `AttMode`, `OneHit`, `FadeOut`, `IsArea`, `Shape`, `RelScl`,
`TarScl`, `TraceTarget`, `ValType` — and all nine live in the single file
`gfx/sgc/刺客_致盲_技能击中.gfx`, the version-4 record that is not an effect
container at all and already fails to parse on purpose. Every key in every effect
file is therefore parsed. `Deviation` and the ten lightning keys (`StepMin`,
`StrokeAmp`, `PerBolts`, …) are parsed and inert, but they occur in 7 roots.

**24 parsed fields have no consumer.** Ranked by how many corpus lines actually
carry a non-default value, because a field that is always zero is inert
harmlessly:

| field | non-default lines | roots | note |
| --- | --- | --- | --- |
| `MaxExtent` | 5,486 | 1,678 | now defaulted, see below |
| `ResetLoopEnd` | 3,657 | 2,300 | `Common`, a flag |
| `MatchGrnd` | 1,781 | — | element+0x131 |
| `IsDummy` | 522 | — | |
| `GrndNormOnly` | 456 | — | |
| `ScaleNoOff` | 249 | — | |
| `vertslife` | 156 | 46 | trail, `%d` |
| `isappend` | 138 | 46 | trail |
| `WriteZ` | 115 | 130 | |
| `GroundHeight` | 28 | — | a *flag* natively |
| `YawEffect` | 30 | — | element+0x195 |
| `Pos1Enable`/`Pos2Enable` | 16 each | 202 | light |
| `MatchSurface` | 16 | — | element+0x170 |
| `EnableMat`, `EnableOrgPos1/2` | 11 each | 990 | ribbon |
| `StrokeAmp` | 11 | 7 | lightning |
| `IsStartOnGrnd`, `Force2D`, `Use3DCam` | **0** | — | always zero in the corpus |
| `NoDownSample` | 1 | 3,552 | |

*(Corrected on 7 November: seven of these 24 have a consumer now. `MatchGrnd`
and `GroundHeight` are honoured through the per-frame `Option<GroundHit>`,
`GrndNormOnly` selects the normal-only ground frame, `vertslife` and `isappend`
drive the trail's vertex ring, `ResetLoopEnd` clears that ring for trails, and
the root key `ShakeCam` — which this table never listed, being a root key —
drives `Player::camera_shake`. `BindEle` resolved by name in the same pass. The
counts above are still the 6 November measurement.)*

`BindEle` and `DummyEle` are `%s` — element **names**, copied to element+0x9c by
`0x7638a0`. 968 elements name a `BindEle`; 772 of those resolve to a sibling
element in the same root and 196 do not (they name `Unnamed1`-style elements that
are not in that root, presumably living in a child). The dominant pairs are
123→110 (140), 120→101 (117), 110→101 (106) and 101→110 (106), i.e. emitters and
sprites binding to each other. `IsDummy` is set on 522 elements but only 2 of
those carry a `DummyEle` that resolves, so dummy binding is close to a no-op in
retail data.

**The gate ladder.** The shared-key loader is `0x6f99f0` and the sprite loader
`0x71df00`; each optional key is behind a `cmp` on the file version. Shared keys:
4, 8, 9, 22, 28, 32, 33, 34, 41, 45, 57, 58, 74, 75. Sprite keys: `ZOffset` 42,
`MatchSurface` 54, `MaxExtent` 55, `YawEffect` 61. The reachable ones are now
honoured by the parser. No corpus root authors a non-default value below its
gate, which is what makes adding them behaviour-preserving on real data while
still being faithful on damaged or hand-edited files.

The format strings also fix each native type: `GroundHeight: %d` is reduced to a
byte by `value != 0` at element+0x132 (a flag, not a height — the parser keeps
the authored number and says so), `RepeatDelay: %d` and `TexAnimMaxTime: %d` are
integers, `MinDist`/`MaxDist`/`TexSpeed` are floats.

**`MaxExtent` is computed, not only authored.** After every gate the sprite
loader runs a tail at `0x71e0ff`: if `MaxExtent` is still zero it becomes
`1.5 * max(Width, Height)`, taking the width branch at `0x71e121` when
`Width > Height` and the height branch at `0x71e141` otherwise. `0x8d75cc` holds
the 1.5. Because the tail is outside the gates it also covers the 3,250 sprites
that carry no `MaxExtent` key at all, so 3,383 corpus sprites were reporting 0
where the client has an extent. Independent confirmation: 3,038 of the 5,486
authored values are *exactly* `1.5 * max(Width, Height)` — the editor writing the
same default back out — and the census's commonest value, 0.750000 on 2,834
elements, is `1.5 * 0.5` for the standard 0.25 × 0.5 sprite.

**`quad_corners` is confirmed from the native side.** The loader derives four
terms into element+0x15c/+0x160/+0x164/+0x168 as `2W*OrgPt1`, `2W - that`,
`2H*OrgPt2`, `2H - that`, and the corner builder at `0x71ea70` multiplies each by
the runtime scale *unless* the matching `NoScale` byte is set — the two bytes are
element+0x151 and +0x152, which is why `no_scale` is a `[bool; 2]`. That is the
split the viewer already used, previously justified only from the static decal
path; the animated sprite path now agrees with it independently. The builder also
multiplies the result by a float at `(element+0x10)+0x1ec` and branches to a
separate path at `0x71f1be` on the byte element+0x148; neither is identified yet.

**`ZEnable` below version 76.** The byte at element+0x130 is written from the
authored value and then forced to 1 by `0x6f9e3a` whenever the version is below
76, so an authored `ZEnable: 0` only survives in a 76..79 root: 334 of the
corpus's 390. `gfx_player::depth_test` and `gfx_decal` already applied that rule
where the quad is built, and it now has a boundary test at 24/37/75/76/79. The
parser keeps reporting the authored value, deliberately: applying the rule twice
would hide it from anything that wants to know what the file said.

**Ground matching is an owner capability, not a file property.** Two small
thiscalls decide where a sprite is anchored, and both read `MatchGrnd`
(element+0x131) and the `GroundHeight` flag (element+0x132) through the object at
element+0x10:

* `0x6fa810` returns `owner+0x1b4` when `owner+0x1f4 && MatchGrnd &&
  (GroundHeight || owner+0x284)`, and `owner+0x1a4` otherwise. The two slots are
  16 bytes apart, i.e. two vectors: the un-snapped position and the ground-snapped
  one. This is the call the `YawEffect` path makes at `0x71f36f`.
* `0x6fa7c0` returns `owner+0xfc` when ground matching applies, `owner+0x13c` when
  `GroundHeight` is set without `MatchGrnd`, and otherwise tail-calls `0x6e5770`.
  The corner builder calls it at `0x71eb3b`, so the four corner terms are applied
  in whichever of those three frames the flags select.

`owner+0x1f4` is **not** the root's `Raytrace`: 454 corpus roots author a
non-zero `MatchGrnd` with `Raytrace: 0` and only one root has both, so reading
`+0x1f4` as `Raytrace` would disable ground matching in almost every file that
asks for it. The object at element+0x10 carries live copies of `DedaultScale`,
`PlaySpeed` and `DefaultAlpha` at +0x1e8/+0x1ec/+0x1f0, which is the layout the
root loader writes, so `+0x1f4` and `+0x284` are either further root fields or
runtime state set after loading; neither is identified. The practical consequence
stands either way: `MatchGrnd` cannot be honoured from the `.gfx` alone, it needs
an owner that can answer a ground query.

That owner now exists. `Player::advance_with_ground` takes an `Option<GroundHit>`
per call — a height and a normal at the placement the effect is parented to — and
`None` is exactly an owner whose `+0x1f4` byte is clear, so the two flags stay
inert for a host with no terrain. `walker/physics.rs::effect_ground` samples the
walker's own map at the owner position `0x6e53f0` raycasts from, one query per
effect per frame, and the ground frame and the height frame the two selectors
choose between are built from that sample. The trait-object version of this
interface was tried and dropped: a `GroundQuery` borrowed from `&MapSpace` cannot
outlive the call, and the per-call `Option` is both faithful and borrow-safe.

**Element offsets, from the clone at `0x70a3d2`.** The copy routine enumerates
the shared header field by field, which is the cheapest complete map available:
+0x34 element count, +0x38/+0x3c/+0x40/+0x48 copied words, +0x9c `BindEle`,
+0xa4 `DummyEle`, +0xb0/+0xb4/+0xb8 `TexRow` and neighbours, +0xc4/+0xc8,
+0xdc `Priority` (confirming the priority gate's read), +0xe0 a 64-byte block
allocated by `0x763250` only when +0x12d is set, +0xf0, and the flag bytes
+0x12c..+0x12f, +0x130 `ZEnable`, +0x131 `MatchGrnd`, +0x132 `GroundHeight`,
+0x134..+0x139, +0x13c. Sprite fields continue at +0x140 `Width`, +0x144
`Height`, +0x148 the byte which, when clear, switches the corner builder to the
ground-matching path at `0x71f1be`,
+0x151/+0x152 the two `NoScale` bytes, +0x154/+0x158 `OrgPt`, +0x15c/+0x160/
+0x164/+0x168 the derived corner terms, +0x16c `ZOffset`, +0x170
`MatchSurface`, +0x174 `MaxExtent`, +0x195 `YawEffect`. The spec payloads
overlay each other, which is why the light writer's `Position: %f, %f, %f` also
reads +0x174.

**`ResetLoopEnd` empties the trail's vertex ring, and restarts the flipbook.**
The shared-key loader reads it behind `cmp $0x4a` — version 74 — and stores
`value != 0` as the byte at element+0x13d. The corpus authors the key on 13,605
lines and obeys that gate exactly: the 3,657 non-default ones appear only in
versions 75, 76, 78 and 79, across 820 roots, and 98 of the archive's 156 trails
are among them.

The flag is read through **vtable slot 17, `+0x44`**, which is how it has direct
callers and none of them are in the effect region. Reading the slot out of each
class's vtable in `.rdata` gives the whole map: base `0x6fa640`, ribbon
`0x70e3c0` (`0x8ec474`, id 110), trail `0x714620` (`0x8ed904`, id 152), child
`0x7121b0` (`0x8ed690`, id 200), emitter `0x715600` (`0x8edab4`, ids 120..125),
and the classes that do not override it at all — sound `0x8ec134` (170), fractal
`0x8ec1d4` (150), curve `0x8ec294` (190), cylinder `0x8ec334` (140), light
`0x8ec3d4` (130), lightning `0x8ec514` (151), model `0x8ed7a0` (160), wave
`0x8edbcc` (180).

The base `0x6fa640` restarts the texture animation when `+0x13d` and `+0x133` are
both set. `+0x133` is not an authored key: the loader derives it, and it is
"this element's flipbook has more than one frame", i.e. `TexRow` and `TexCol` are
not both 1. So for a sprite the flag means *rewind the flipbook at the loop
boundary*, and for the classes that override the slot it means whatever their own
override does.

The trail's override `0x714620` zeroes element+0x314, +0x318 and +0x328 when the
flag is set and returns immediately when it is not. Those slots are the trail's
own vertex ring, which the initialiser at `0x713814` sets up as capacity `0x100`
at +0x31c, head `0xff` (= capacity − 1) at +0x324, count at +0x328 and storage at
+0x320, and which the fill loop at `0x713ed4` writes as
`buffer + head*12 = { index, element+0x30c, 0 }` for each of `element+0x2b8`
entries: +0x30c is the authored `VertsLife` and +0x2b8 the segment count. The
record is three integers, not three floats, and the ring is a trail — the earlier
reading of it as a pending-spawn queue was wrong, and it is what made the flag
look unactionable. `ResetLoopEnd` appearing on 18 element ids is not evidence
against that: 17 of them take the base or their own class's override, and only
152 clears a ring.

`gfx_player::reset_at_loop_end` implements the trail reading — the ring's laid
count, vertex count and accumulator go back to zero, so a looping trail relays
from its start instead of continuing to wind — and documents the flipbook reading
beside it. The base needs no code in the viewer: its flipbook index is derived
from the element clock, which a restart already rewinds, so the half of `0x6fa640`
that zeroes the texture-animation clocks at +0xac, +0xbc, +0xc0 and +0x128 has
nothing to act on. The ribbon, child and emitter overrides stay unreproduced, for
the reason recorded in `reset_at_loop_end`: none of the three tests
`ResetLoopEnd` itself and the slot is reached through a computed pointer, so
whether the caller gates the whole hook is not recovered. `Player::reset` still
keeps live particles across a loop restart, which is what the client does.

Two further offsets fell out of the same reads: `NoDownSample` is the byte at
element+0x134 and `TexAnimMaxTime` the integer at element+0x124, behind version
75. Both are consistent with the clone's field list.

**`IsAvgGen`, `AlphaSeg` and `BetaSeg` are a much smaller gap than the census
suggested.** They are read by a separate loader behind `cmp $0x47` — version 71 —
into element+0xe4 (`IsAvgGen`, reduced to a bool by `setne`), +0xe8 (`AlphaSeg`)
and +0xec (`BetaSeg`), the latter two `sscanf`ed straight into the element as
`%d`. The corpus obeys the gate exactly: all three keys occur only in versions
71..79. `AlphaSeg` and `BetaSeg` are on element 123 alone (6,641 each) and are
**10 in 6,604 and 6,607 of those** — a fixed setting, not a varied one.
`IsAvgGen` also occurs on element 125 (373) but is **non-zero on only 84 elements
in the whole archive**. So the behaviour these three gate is worth 84 elements,
not the 6,641 the raw key count implies, and the segment counts are near-constant
where they do appear.

**Binding by name: the three string slots, and the only comparison that exists.**
The element constructor at `0x6f9184`..`0x6f91b7` builds three string objects back
to back — `+0xa4` with mode 3, `+0x9c` with mode 2, `+0x94` with mode 1 — which
fixes **Name at element+0x94, `BindEle` at +0x9c and `DummyEle` at +0xa4**, eight
bytes apart, so they are string objects rather than inline buffers (the corpus has
names like `Unnamed12` that would not fit in eight bytes).

The whole binary contains exactly three calls to the string compare `0x763c30` in
the effect region. Two of them, `0x7027db` and `0x70286c`, are generic
name-to-index lookups — one over a caller-supplied array of 8-byte records, one
over a static table of 0x1c-byte records at `0x8ec898` — and neither touches an
element. The third, `0x6e456e`, is the binding pass:

```
container+0x0c = array of element pointers, container+0x18 = count
for i in 0..count:
    e = array[i]
    if e == argument:            index = i          # find myself
    elif compare(*(argument+8), e+0x9c):            # my Name vs their BindEle
        0x74c2a0(e, 0)                              # tell them, with a null target
```

So it is the **detach** direction: given an element, every element whose `BindEle`
names it is handed a null. The function runs inside a structured-exception block
(handler `0x8c7fa8`) and first calls `0x6e5900` with `container+0x1c`.

What is *missing* is the attach direction. No site in the effect region compares
anything against `Name` at +0x94 to resolve a `BindEle` into a target pointer, so
either the attach uses a different compare routine or the index is built once at
load and only ever torn down through the pass above. That is where the next dig
starts. Until it is settled, the 772 corpus elements that bind to a resolvable
sibling stay unbound in the viewer: an emitter that should follow a sprite is
spawned at its own origin instead, which is the largest remaining behavioural
difference and the one most likely to be visible.

Two traps worth recording for the next dig. The parser's format strings live in
`.data` and are reached through arrays of pointers, so a key's string has **no
direct cross-reference** — find the array by scanning the image for words equal
to the string's address, then grep for the array slot. The writer's strings are
in `.rdata` and are pushed directly, which is how the light-element writer at
`0x70b7e1` turned up and why its `Position: %f, %f, %f` at element+0x174 is not
`MaxExtent`: the spec structs overlay the same offsets. And `[esp+0x938]` next to
`[esp+0x944]` looked like two different version values until the three words
pushed for the `sscanf` call were counted; both are the same argument. And a third
trap, earned while chasing the binding attach: **offsets collide across unrelated
classes**, so an offset identifies a field only once the enclosing function is
known to operate on that type. `+0xe0` is a 64-byte allocation on an element but a
vtable store (`movl $0x8eb938,0xe0(%esi)`) on the objects at `0x6e6a7d` and
`0x6e6fcb`; `+0x94` is the element `Name` but an integer field on several unrelated
structs in the `0x6e1xxx` and `0x6f3xxx` blocks. Grepping an offset across the
whole effect region therefore produces mostly noise — grep for the *call sites* of
a helper instead, which is how the single `BindEle` comparison was found.

## Still open

Unchanged from the previous write-up and not addressed here: the interpolation and octave weighting inside the fractal-noise generator `0x766490` behind controllers 107, 109 and 112 — the shake now samples the viewer's own `fbm` in its place, see *Camera shake, reproduced* — the cross-product side vector inside the element 190 curve builder (the class now draws; see `Every-effect-2026-11-08.md`), models/sounds/lights belonging to other subsystems, and the 82 child references to files that do not ship. The `gfx_light` millisecond window listed here previously is fixed in *Fixed while writing this up*; the ramp time-base question that fix opened is still unsettled and needs the controller-108 update routine.

Landed since the last revision of this list, and therefore no longer open: binding by name, `MatchGrnd` and `GroundHeight` through the per-frame ground sample, `GrndNormOnly`, the trail's `isappend`/`vertslife` vertex ring, `ResetLoopEnd` for trails, and `ShakeCam`. *Placed in the viewer on 7 November* is the summary.

Two things the shake left open, both recorded where the code can be checked against them:

* **Which rotation the client composes first.** The two `rand()` draws become axis-angle quaternions about the unit Y and unit X axes and are multiplied by `0x4071f0`, but the order is not readable out of the helper sequence, so `shake_direction` builds a fresh random unit direction from the same two angles. Both readings cover the sphere; they are not the same sequence of directions.
* **Nobody holds the camera yet.** `Player::camera_shake` returns the offset, `CharacterGfx::camera_shake` sums the roots an effect list plays and `Walker::camera_shake` exposes that — but the walker's host builds the frame's view-projection outside the walker, in `gpu.rs`, so the offset is computed and returned and nothing adds it to the eye. That is one addition in the host's camera, and it is the only step between this and a screen that shakes.

Added by the field census, in the order the corpus says they matter:

* **`ResetLoopEnd` for the other four classes.** The trail's override is
  implemented; the base `0x6fa640` restarts the flipbook when element+0x133 says
  the element has more than one frame, and the ribbon `0x70e3c0`, child
  `0x7121b0` and emitter `0x715600` overrides are decoded as far as their slot
  but not read. 3,657 non-default lines across 820 roots, of which the trails are
  98 elements.
* **`IsDummy`** (522 lines) marks an element as a placement only. Nothing in the
  effect region skips drawing one, so the guess that it suppresses rendering is
  unconfirmed and it stays parsed and inert.
* **`WriteZ`** (115 lines in 130 roots, on model references) and **`MatchSurface`**
  (16 lines, element+0x170) have no recovered reader.
* **`YawEffect`** (30 lines) selects the rotation path at `0x71f36d` and
  `0x71f84e`, which calls `0x6fa810` and `0x7872d0`; neither is decoded.
* **`Pos1Enable`/`Pos2Enable`** (16 lines each in 202 light roots) and the ribbon's
  **`EnableMat`/`EnableOrgPos1/2`** (11 lines each in 990 roots) gate keypoint
  channels that the viewer reads unconditionally.
* **`ScaleNoOff`** (249 lines) and **`StrokeAmp`** (11 lines in 7 lightning roots).
* The unidentified multiplier at `(element+0x10)+0x1ec` and the byte at
  element+0x148 that switches the corner builder to `0x71f1be`.
* `IsAvgGen` is worth 84 elements, not the 6,641 its key count suggests, and
  `AlphaSeg`/`BetaSeg` are 10 almost everywhere they appear. Demoted from the top
  of this list by measurement.
* `IsStartOnGrnd`, `Force2D`, `Use3DCam` and `NoDownSample` are parsed and inert,
  but every corpus line has them at their default, so nothing is lost until a
  modded file appears.
