# Sky texture selection (P5.04, and the P5.09 seam question)

This one could not be settled without `textures.pck`, which is now in the
workspace (117,047,648 bytes, 2019-05-03 build, matching this client).

## The sky set is configuration, not filenames

The viewer previously hardcoded three folders and three filenames:

```rust
let names = ["clear", "evening", "night2"];
for name in ["01.bmp", "02.bmp", "roof.bmp"] { ... }
```

The client does not do this. `CECConfigs::Init` reads named settings, and the
retail `configs/element_client.cfg` shipped in `configs.pck` states them
outright:

```ini
[DefEnv]
sky_image01 = Sky\clear\Roof.bmp
sky_image02 = Sky\clear\01.bmp
sky_image03 = Sky\clear\02.bmp
```

Two immediate consequences:

1. **`01.bmp` precedes `02.bmp`.** The horizontal order is roof, 01, 02 - the
   natural reading and the one the viewer already assumed.
2. **The selection is data.** A region can point `sky_image0*` at any folder,
   and clicking through the archive shows why a filename pattern cannot be
   assumed: of 59 sky folders, only **16** contain the canonical
   `01.bmp`/`02.bmp`/`roof.bmp` triple. The rest use variant names -
   `roog.bmp`, `wwwww.bmp`, `1.bmp`, `sky_spire.bmp`, `sky_c1.bmp`,
   `堂密林天空顶.bmp` - and at least one region set (`a满堂天空`) names all three
   differently again. A pattern-based loader silently loads the wrong sky for
   43 of 59 folders; a config-based loader does not.

## The seam question: settled as "keep the crossfade"

`01.bmp` and `02.bmp` are 512x512 24-bit uncompressed BMPs. Measuring the
interior of each image gives the noise floor: adjacent columns differ by a mean
abs delta of about **0.25** per channel.

Cross-edge comparisons, same metric:

| pairing | mean abs delta |
|---|---:|
| `01` right edge to `02` left edge | 8.76 |
| `01` left edge to `02` right edge | 4.15 |
| `01` left edge to `01` right edge | 4.63 |
| `02` left edge to `02` right edge | 8.31 |

Every cross pairing is 15-35x the noise floor, so **the halves do not tile**:
there is a real discontinuity where one panorama meets the other. That
vindicates the border crossfade in `sky()` and definitively retires the earlier
"the seam samples the wrong panorama" theory - the seam is genuine authored
content, not a sampling bug.

Note also that `01R->02L` (8.76) is *worse* than `01L->02R` (4.15), which at
first looked like evidence the halves were stored in reverse. The config settles
it the other way: 01 really does precede 02. The pixel asymmetry is simply how
the artwork is painted, and it is a good illustration of why a metric on one
column pair cannot outrank a stated setting.

## What is implemented from this

`SkyGpu` now reads `sky_image01/02/03` from `configs/element_client.cfg`
(`[DefEnv]`) and resolves those paths case-insensitively through the pack,
falling back to the documented `Sky\clear` defaults only when the config or the
listed member is absent.

## Still open under P5.04

The *per-region* selection. `element_client.cfg` carries the default set, and
the archive carries 59 alternative sets, but which region selects which set is
in the region environment data, not in this file, and that mapping has not been
recovered. The viewer therefore renders the configured default set everywhere.
That is a real remaining gap and is recorded as such rather than implied closed.

## Open: what `roof.bmp` actually projects onto

`sky_image01` is named "Roof" and the viewer treats it as a zenith cap: it maps
the ray's `xz` direction into a disc and fades it in above `smoothstep(0.70,
0.97, dir.y)`. The archive suggests the client does something more specific, and
this is **not** settled here.

Every one of the 223 sky BMPs is 512x512 (201 in 24-bit, 22 in 32-bit), so
nothing distinguishes roof from the halves by shape. But `clear/roof.bmp` is not
the radially symmetric image a naive top-down cap would be:

* corner samples are all essentially identical, `(92-93, 129-130, 190-191)`;
* a 32x32 centre patch averages `(178, 204, 230)` against `(119, 151, 201)` for
  the top strip, so it is bright in the middle and dark at the rim - consistent
  with a dome;
* yet the four quadrants differ materially: TL `(141,169,211)`, TR
  `(133,162,207)`, BL `(166,187,220)`, BR `(149,174,214)`.

A radially symmetric cap cannot have quadrant means that differ by 33 levels in
red. So `roof.bmp` carries directional structure, and the true projection is not
a plain disc map. Candidates are a cubemap face, an equirectangular continuation
of the two halves, or a dome cap whose u is still tied to the panorama's
azimuth. Choosing between them needs the client's roof sampler, which has not
been located, so the viewer keeps the disc approximation and this stays open.

Recorded as a limitation rather than quietly left in: the roof path currently
uses the right texture for the wrong reason - it is configured correctly now,
but how it is projected is still an approximation.
