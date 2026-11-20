# P5.01–P5.03 — Sky geometry, day/night switch, sun and moon

> **Update 2026-11-04 (later):** the celestial sizes in section 3 are **wrong**
> and are corrected here. `CECSunMoon::Init` fills the vertex streams with
> explicit quad templates — 20 x 20 units (sun), 120 x 40 (sun shine) and
> 40 x 40 (moon) in a plane 100 units from the camera — so the native quads
> subtend 11.42 and 22.62 degrees, not 1.146. The `ds:0x8d6a94` / `ds:0x8d6a98`
> pair the old extent was inferred from is not referenced by the celestial code
> at all. See `Sun-moon-flare-cloud-weather-rotation-2026-11-04.md`.
>
> **Update 2026-11-04:** the viewer now reads `configs.pck` with the new
> `angelica_pck` reader, loads the `sky.lst` day/night pair (clear / overcast)
> and draws the client's flare chain. Section 2.3 and 3.2 below are corrected
> in place; see `Sky-parity-checkup-2026-11-04.md` for the checkup and the
> changed files.

Agent 5-A. Evidence: `elementclient.exe` (Drive id `19h1s6U7JR0uvVccqeL8ZTAVpVeal1X1X`,
6,000,640 bytes, image base `0x400000`) plus the `textures.pck` asset names in the
client config. Every address below is a virtual address in that image.

Nothing in this report is inferred from the look of the artwork: geometry comes
from executing the original builders under Unicorn, and the time rule comes from
the original update functions.

The client's own `configs.pck` (834,575 bytes, 67 entries) and `shaders.pck`
(8,225,973 bytes, 371 entries) were also decoded; section 0 records the container
format and `tools/extract-angelic-pck.py` reproduces the extraction. That closed
the three questions this report previously listed as blocked on them: which
shader (if any) covers the empty band, which `Textures\Sky` folders the day and
night sets name, and what the celestial material states and flare pipeline are.

---

## 0. The shipped config and shader archives

`configs.pck` and `shaders.pck` are **Angelica File Package** (`0x00020002`) files,
not the retail v2 PCK that `src/pck.rs` reads. They share the 276-byte record
layout (name, `u32` offset, `u32` size, `u32` compressed size at `+260/+264/+268`)
but not the trailer:

| Offset | Content |
| --- | --- |
| `0x000` | `u32 0x4dca23ef`, file size, `u32 0x56a089b7` |
| `0x00c` | file payloads; a stored file is raw bytes, a compressed one a bare zlib stream |
| — | directory: per entry an 8-byte header and a zlib stream of one 276-byte record |
| `-280` | `u32 0xa508bdc4`, `u32 version`, `u32 unknown` (configs `0x21cfb4f6`, shaders `0x21be2dbf`), ASCII `"Angelica File Package, Perfect World..."`, `u32 0x853567c8` at `+268`, entry count at `+272`, version again at `+276` |

`textures.pck` is the same format with a **different guard pair**: `0xfdfdfeee`
at `-280` and `0xf00dbeef` / entry count / version in the last twelve bytes
instead of `0xa508bdc4` / `0x853567c8` at `-280`/`-12`. The record layout,
directory layout and payload layout are otherwise identical, so
`tools/extract-angelic-pck.py` accepts either pair and now reads the texture
archive too (1038/1038 entries, `"Angelica File Package, Perfect World."`). The
directory walk also skips offsets whose zlib header cannot be a deflate stream
(`CM != 8`), which keeps the 1038-record scan fast.

`tools/extract-angelic-pck.py` validates the trailer, finds each directory record
by the zlib stream that ends exactly at the running position, and reads entries by
name — `--list`, `--cat <name>`, `--out <dir>`. All three archives decode
completely (67/67, 371/371, 1038/1038). `src/pck.rs` is unchanged: it still
targets the retail archive the viewer loads `textures.pck` from.

---

## 1. Geometry and projection (P5.01)

### 1.1 Addresses

| Item | Address | Notes |
| --- | --- | --- |
| `A3DSkySphere` constructor | `0x799610` | stores `+0x4c0` = 6 bands, `+0x4c4` = `0x3e9c61aa` (0.30543262 rad), `+0x4c8` = 50.0 |
| Horizon curtain builder | `0x79a330` | writes stream `this+0x584`; index stream `this+0x5a0`; counts `+0x58c`/`+0x590` |
| Polar cap builder | `0x79a810` | writes stream `this+0x588` |
| Cap draw | `0x799a40` | binds the `+0x504` or `+0x544` transform, cap texture `+0x54` |
| Curtain draw | `0x799b40(textA, textB)` | two passes over the whole buffer, second pass under a `z`-mirrored world matrix |
| Sky tick / commit | `0x79a040`, `0x79a0e0`, `0x79b150`, `0x79b180` | timer and deferred set swap (section 2) |
| Texture paths | `0x79ac6c`, `0x79ac80`, `0x79ac97`, `0x79aee8`, `0x79af06`, `0x79af1d` | `Textures\Sky\%s` |
| Texture creation | `0x7997a0` | `+0x5ac`, `+0x5a4`, `+0x5a8`, `+0x54`; early-out if `ds:0x988334 == 4` |

Assets: `A3DSkySphere::TransSky(this, roof, 01, 02, 0)` (`0x79ac30`) loads exactly
the three names the caller supplies through the format string `Textures\Sky\%s`.
The default environment in `element_client.cfg` (`[DefEnv]`) supplies
`Sky\clear\Roof.bmp`, `Sky\clear\01.bmp`, `Sky\clear\02.bmp`, which resolve to
`textures/sky/clear/{roof,01,02}.bmp` — the folders present in the packed
`textures.pck`. `autofamilyconfigs/sky.lst` holds the same triple for each named
set (section 2.3). The cap samples `roof.bmp`, the two curtain islands sample
`01.bmp` and `02.bmp`.

### 1.2 Curtain — 70 vertices, 288 indices, 96 triangles

Rows are `39.27 * [1, 0.5, 0.167, 0, -0.27]`; each row holds two 7-column islands
(azimuth step 15 degrees), `u = 1 - column/6`, position `50 * (cos a, 0, sin a)`.

| row | y | front island `v` (az 90…180) | back island `v` (az 0…90) | diffuse |
| --- | --- | --- | --- | --- |
| 0 (rim) | 39.2700005 | 0.05 | 0.95 | `0x00ffffff` (alpha 0) |
| 1 | 19.6350002 | 0.275 | 0.725 | `0xffffffff` |
| 2 | 6.5580897 | 0.42485 | 0.57515 | `0xffffffff` |
| 3 | 0.0 | 0.5 | 0.5 | `0xffffffff` |
| 4 | -10.6029005 | 0.5 | 0.5 | `0xffffffff` |

Each row's two island runs sum to one texture height (`v` and `1 - v`); the two below-horizon
rows both sit on the midline `v = 0.5`, so the image's lower half is never sampled there.

Elevation coverage, as seen from the sphere centre: the curtain is a cylinder of radius 50, so a ray meets it at `50 * tan(elevation)`; the rows span **-11.98° to +38.16°**. Quads are
`(v0, v0+14, v0+15), (v0, v0+15, v0+1)`; all 96 triangles have
`normal · centroid < 0`, i.e. they face the camera inside the sphere.

### 1.3 Cap — 145 vertices, 792 indices, 264 triangles

Pole `(0, 50, 0)` at `uv (0.5, 0.5)`, then 6 rings of 24 sectors:
`theta = 0.30543262 * ring/6`, `phi = 2*pi*sector/24`,
`(50 sin theta cos phi, 50 cos theta, 50 sin theta sin phi)` and the planar disc
mapping `u = 0.5 + x / (2*50*sin(step))`, `v = 0.5 + z / (2*50*sin(step))`.

| ring | y | ring radius |
| --- | --- | --- |
| 1 | 49.93523 | 2.54417 |
| 2 | 49.74109 | 5.08175 |
| 3 | 49.41809 | 7.60616 |
| 4 | 48.96709 | 10.11176 |
| 5 | 48.38912 | 12.58944 |
| 6 | 47.68585 | 15.03530 |

Elevation coverage: **72.5° to 90°**. Indices: pole fan `(0, i, i+1)`, then
`(a, c, d), (a, d, b)` per sector; all 264 triangles have a positive `x/z`
signed area.

### 1.4 Joins

* **Island join at azimuth 90°** — the same point is stored twice: front island
  column 0 has `u = 1`, back island column 6 has `u = 0`. The rasteriser never
  interpolates across the join, so the texture run reverses exactly at the
  meridian. Verified in `island_join_is_a_duplicated_column_with_a_reversed_texture_run`.
* **Meridian at azimuth 0°/180°** — no duplicate column exists. The second draw
  feeds the identical vertex with `z` mirrored, so the ring closes with the same
  `u` on both sides. Verified in `meridian_join_reuses_one_vertex_for_both_draws`.
* There is **no crossfade in the client anywhere**. The seam handling is
  "separate texture per half space, exact vertex reuse at the mirror plane". The
  previous `u < 0.08` / `u > 0.92` crossfade in `src/sky_gpu.rs` was removed with
  the approximation it propped up.
* **Viewer additions after the test rounds.** The ray projector samples both
  texture runs and crossfades them over 3.4° of azimuth around the 90° island
  join (`1 - smoothstep(0, 0.06, |az - pi/2|)`); with the shipped `clear` art
  the visible cloud seam turned out to sit on the **mirror plane** instead, so
  the two panorama textures (`01.bmp`, `02.bmp`, which the curtain draw passes
  as two stages) now crossfade over 7° of azimuth across `|d.z| -> 0`
  (`1 - smoothstep(0, 0.12, |d.z|)`, 50/50 exactly on the plane). Each half
  space keeps one texture as its primary, so the art is still shown at full
  contrast; only the join blends.

### 1.5 The band the native buffers leave open

The curtain rim is at 38.16° and the cap rim at 72.5°, so **38.16°–72.5° has no
geometry in either stream**. The native rim vertex carries alpha 0, so the
client shows the pass background there. The renderer reproduces exactly that:
`sky_geometry::project` returns
`Region::CapEdgeGap` with `alpha = 0` for the
band and the shader mixes towards the opaque-pass clear colour
(`0.18, 0.32, 0.48`, the value `draw_opaque` uses when it draws the sky).

**Nothing in the client fills the band.** The two draw paths
(`0x799a40` cap, `0x799b40` curtain) bind the transformed stream and set
fixed-function state; neither selects a vertex program, and the client's only
shader archive contains no sky program at all — `shaders.pck` holds 371 entries
(`moon.sdr`, `sun.sdr`, `sunshine.sdr`, `ps/solarflare_14.txt` and effects), and
`A3DSky`/`A3DSkySphere` never appear in it. The band is therefore a property of
the shipped buffers, not of a program this repository cannot read. What the
retail game may have drawn *over* it (clouds, fog) is not part of these two
objects and is not established here.

Four test rounds shaped the viewer's fill of this band:

1. a single fixed colour read as a **blue ring**;
2. a gradient to the cap's rim colour sampled on *one circle* read as **radial
   streaks bleeding outward** (one colour per azimuth, stretched over 34° of
   elevation);
3. a sweeping radius sampling the roof texture's *mid* region read as a **cloud
   ring with radial bleed** — the sampled art was not the art the cap ends with,
   so the join at 72.5° was a visible edge;
4. the shipped fill: **the cap's own lookup, evaluated at every elevation.**

The fourth version is the one that cannot produce either artifact, because it is
not a second surface at all. `A3DSkySphere::Init` maps the cap with
`uv = 0.5 + (x, z) / (2 * R * sin(step))`, i.e. `uv = 0.5 + d.xz * 1.66275358` in
ray terms. At the cap rim that radius is exactly `0.5` — the roof texture's own
edge — and it grows monotonically as the elevation drops: `0.5` at 72.5°,
`1.307` at 38.16°. So continuing the same expression across the band gives a
lookup that is (a) pixel-continuous with the cap at its top, (b) one smooth
function of the direction, so it can hold neither a fixed colour nor a single
circle, and (c) art the cap already shows rather than a new invention.

Two details make it work:

* **The roof texture tiles.** `clear\roof.bmp`'s opposite borders differ by
  `0.6`/`2.5` levels against a mean neighbour step of `1.3`/`1.7`, i.e. by less
  than pixel noise — the sheet is authored to be sampled past one tile. The
  shader therefore binds a second sampler with `AddressMode::Repeat` for the
  roof layer only (the panoramas and the discs keep clamp), which is also what
  the engine's own wrap addressing does. With clamp the border pixel is smeared
  along the rim, which is the vertical bleed the third round reported.
* **The skirt fades over the sheet.** The curtain's rim row carries vertex
  alpha 0 and reaches 1 at `y = 19.635`, so its authored fade is an alpha ramp.
  The shader now composites the curtain *over* the roof sheet
  (`mix(roof, curtain, curtain_alpha)`) instead of fading both into the pass
  clear colour. Fading into the clear colour is what drew a dark ring under the
  panorama rim, because the clear colour is darker than any of the art.

`sky_geometry::project` reports the band as `Region::CapEdgeGap` with the same
lookup and `alpha = 1`, and the test `the_open_band_is_the_cap_surface_continued`
pins monotonicity, the twin joins, the `0.5`-at-the-rim continuity and the fact
that the radius passes one whole tile at the bottom of the band.

This is still a viewer-only composition: the object retail draws there is
`0x799f88` (`[A3DSky+0x54]`, vtable `+0x1c`, between the cap and curtain draws)
and is not identified. What is recovered is the texture it uses — the same
`clear\roof.bmp` — and the projection the cap that shares that texture uses.

---

## 2. Day/night rule (P5.02)

### 2.1 Addresses

| Item | Address | Content |
| --- | --- | --- |
| Clock | `0x4b5f10` | `+0x68 += ms * ds:0x8db108` (double `6.944444e-8` = 1/14,400,000), wrapped at `ds:0x8d6138` = 1.0 |
| Authored time | `0x4b6030` | `+0x68 = wrap(time)`, then update |
| Update | `0x4b60e0` | `+0x4c` sun elevation, `+0x50` moon elevation, `+0x60` azimuth, `+0x70` night strength, `+0x74` night flag, `+0x54/+0x58/+0x5c` direction |
| Sun direction | `0x4b60b0` | `+0x60 = atan2(-z, -x)` from the region `sun_dir` |
| Set selection | `0x4203e0` | 8-way branch on `BYTE[esp+0x19c]`, `[sunmoon+0x74]` vs `0.0/1.0/0.8`, texture-coord vs `0.0/0.5`; each arm `0x79ac30(this, a, b, c, 0)` (texture set) and then `0x79aeb0(this, a, b, c, time)` (lighting record) |
| Switch length | `0x4b6250` | returns `0x249f00` (2,400,000) if `[+0x74] != 0`, else `0x1b7740` (1,800,000); caller halves it |
| Timer start | `0x79b150(this, n)` | `+0x5b0 = n`, `+0x5b4 = n` (clamped >= 1), `+0x5b8 = 1` |
| Timer step | `0x79a040` | `+0x5b4 -= 0x21` while `+0x5b8`; interpolates `(+0x504, +0x4cc)` and `(+0x544, +0x4e8)` through `0x79a0e0` with the same weight; commits through `0x79b180` at zero |
| Commit | `0x79b180` | releases `+0x5ac`, `+0x5a4`, `+0x5a8`, repopulates from `+0x8c8`, `+0x8cc`, `+0x8d0`, copies `+0x4e8 → +0x4cc` and `+0x544 → +0x504` |

### 2.2 Recovered schedule (`t = hour/24`)

| hours | sun elevation `+0x4c` | moon elevation `+0x50` | night flag `+0x74` | night weight |
| --- | --- | --- | --- | --- |
| 00:00–03:30 | 0 | `(1 - 6.8571429 t) * 60°` (60° → 0°) | 0 | `1 - t/0.0625` (1 → 0) |
| 03:30–12:00 | `(t - 0.14583333) * 197.6°` (0 → 70°) | 0 | 0 | 0 |
| 12:00–21:00 | `(1 - (t - 0.5) * 2.6666667) * 70°` (70° → 0°) | 0 | 1 from 18:00 | `(t - 0.75)/0.0833` from 18:00 (0 → 1) |
| 21:00–24:00 | 0 | `(t - 0.875) * 480°` (0 → 60°) | 1 | 1 |

`+0x70` night strength: 1.75 at midnight, 0 at 03:30, 0 through the day, 0 → 1
from 18:00 to 21:00, 1 to midnight. It is deliberately discontinuous at midnight
(1.0 → 1.75); `elevation_ramps_are_continuous_apart_from_the_midnight_strength_step`
pins both facts.

Switch lengths: night 2,400,000/2 = **1,200,000 clock-ms = 2 h** (18:00 → 20:00),
day 1,800,000/2 = **900,000 clock-ms = 1.5 h** (00:00 → 01:30). Because the timer
is proportional to the clock, the blend weight is a pure function of the hour:

```
night_weight(hour) = 1 - t/0.0625            t < 0.0625
                   = 0                       0.0625 <= t < 0.75
                   = (t - 0.75)/0.08333     0.75 <= t < 0.83333
                   = 1                       t >= 0.83333
```

This replaces the previous `smoothstep(5,8,hour)` / `smoothstep(17,20,hour)`
schedule. The shader no longer contains a day/night curve at all: `SkyFrame` is
filled from `sky_time::night_weight`, and the shader mixes the two recovered
texture sets with it. The set itself is picked by `+0x74` (18:00 today,
midnight back), which `night_flag_is_the_texture_set_selector` pins.

### 2.3 Where the texture names come from

`0x4203e0` calls `0x79ac30(this, roof, first, second, 0)` — `TransSky` — with the
three paths of the set it wants; `TransSky` then loads `Textures\Sky\%s`
(`0x79ac6c`, error string `A3DSkySphere::TransSky(), Can't load texture [%s]`) and
`0x79b180` commits the new set at the end of the timer. The paths themselves are
configuration data:

* `element_client.cfg`, `[DefEnv]` — the default environment:
  `sky_image01 = Sky\clear\Roof.bmp`, `sky_image02 = Sky\clear\01.bmp`,
  `sky_image03 = Sky\clear\02.bmp`.
* `autofamilyconfigs/sky.lst` — the named sets, 3 rows of `index, name, roof, 01, 02`:

| index | name | roof | first island | second island |
| --- | --- | --- | --- | --- |
| 0 | 晴天 (clear) | `clear\roof.bmp` | `clear\01.bmp` | `clear\02.bmp` |
| 1 | 阴天 (overcast) | `阴天01\roof.bmp` | `阴天01\01.bmp` | `阴天01\02.bmp` |
| 2 | 夕阳 (sunset) | `夕阳\wwwww.bmp` | `夕阳\01.bmp` | `夕阳\02.bmp` |

So a set is three configured paths and `sky_id_day`/`sky_id_night` select which
set a region asks for. The `element_client.cfg` `[DefEnv]` default names
`Sky\clear` for all three `sky_image` slots, but the scene-level
`autofamilyconfigs/autoscene.cfg` overrides that with `sky_id_day = 0` and
`sky_id_night = 1`: the night sky is the **overcast** set (`阴天01`), not the
clear set and not the legacy `night2` folder. What changes at dusk is the
selected set plus the lighting (`+0x70` strength, the terrain and model
day/night switch) and the rim alpha.

**Corrected 2026-11-04.** The earlier conclusion that "one authored set covers
both phases" was based on `element_client.cfg` alone and missed
`autoscene.cfg`. The viewer now reads `configs.pck` with `angelica_pck` (the
retail reader rejected its guard), loads all three `sky.lst` rows in file
order, and installs `sky_id_day`/`sky_id_night` into the transition machine.
The night look is the overcast set modulated by the uniform `tint`: the
recovered regional light colour (`max(ambient, diffuse)`) times
`1 - 0.72 * night_weight`, where `night_weight` is the recovered switch weight
of section 2.2. The light colour alone was not enough — a region without a
lighting record left the night sky at full daylight — and the switch weight is
the one recovered number that says how far into the night the sky clock is.
The switch timer is unchanged. See `Sky-parity-checkup-2026-11-04.md` for the
full checkup.

---

### 2.4 What the shipped sky art actually contains

Read out of `textures.pck` (all `512x512x24` BMPs, 786 488 bytes, no mip chain):

| File | Content |
| --- | --- |
| `textures/sky/clear/01.bmp` | day sky; the **bottom half is a mirror** of the top half (the two 90° islands of the mesh), so the texture is one 180° picture used twice |
| `textures/sky/clear/02.bmp` | a second day sky with a horizon haze band, same mirrored-halves layout |
| `textures/sky/clear/roof.bmp` | a square zenith view (clouds), the cap disc samples its inscribed circle |
| `textures/sky/night2/01.bmp` | night sky **with a painted moon** in its lower half |
| `textures/sky/trans.tga` | `256x1` white ramp, `RGB = alpha = u * 255` |
| `textures/nature/sun.bmp` | `256x256`, white disc with glow; the solid disc is ~55 % of the quad |
| `textures/nature/moon.tga` | `512x512`, moon filling ~75 % of the frame plus a faint glow |
| `textures/nature/sunshine.bmp` | `256x128` (2:1) streak, the anamorphic part of the flare |
| `textures/nature/sunflare.bmp`, `moonflare.bmp` | `128x128` and `256x256` flare sprites |

Two consequences that shaped the third round:

* **The painted moon is `night2/01.bmp`.** `element_client.cfg` names
  `Sky\clear` for all three `sky_image` slots, so a faithful default draws no
  painted moon at all; the viewer's earlier `night2` substitution is what put a
  motionless moon next to the billboard. It stays removed.
* **The billboard is not small.** Corrected 2026-11-04: the quad templates are
  20 x 20 units for the sun and 40 x 40 for the moon at distance 100, i.e.
  11.42° and 22.62° (and the sun swells by `1 + 2 * night_strength` at dusk).
  `moon.tga`'s alpha fills 83.6% of its quad, so the retail moon disc is 18.9°;
  `sun.bmp`'s 50%-peak contour is 60% of its quad, so the retail sun disc is
  6.9° inside an 11.42° glow. The 1.146°/0.63°/0.86° numbers this bullet used to
  carry came from `ds:0x8d6a94`/`ds:0x8d6a98`, which the celestial code never
  reads. See `Sun-moon-flare-cloud-weather-rotation-2026-11-04.md` §1.

---

## 3. Sun and moon (P5.03)

| Item | Address | Content |
| --- | --- | --- |
| Render | `0x4b3e00`–`0x4b41ee` | one 4-vertex, 6-index billboard per object (`push 0x6` at `0x4b401a`) |
| Gate | `0x4b4035` | `fcomp ds:0x8d58a0` (0.0) on the object's own elevation |
| Placement | `0x4b3f69`–`0x4b3fb6` | `direction * ds:0x8d6a9c` = **100.0** |
| Extent | `0x4b2fde`..`0x4b30f9` (sun fill), `0x4b3425`..`0x4b3517` (moon fill) | sun `±10` x, `0..-20` y; moon `±20` x, `0..-40` y — **20 x 20 and 40 x 40 world units**; `0x8d6a94`/`0x8d6a98` are not referenced by this code |
| Direction | `0x4b4085`–`0x4b40b1` | `(cos az cos el, sin el, sin az cos el)` |
| Alpha | `0x4b405f`–`0x4b4080` | packed diffuse `([+0x48] & 0xfefefe | 0x01010100) >> 1 | fade << 24` |
| Fades | `0x4b5f63`–`0x4b6021` | `+0x7c` sun, `+0x80` moon, `± ms * ds:0x8db100` (0.0003), clamped to [0, 1]; targets `+0x84`/`+0x85` |
| Textures | — | `textures/nature/sun.bmp`, `textures/nature/moon.tga` |

Angular size: `2 * atan(1/100)` = **1.1459°** full quad, half-angle 0.01 rad.
This is the whole recovered billboard, so the round-two complaint that the discs
are tiny is a statement about the billboard, which is correct: what makes the
retail sun read larger is the `sunshine.bmp` shine quad of section 3.2. That
quad is now drawn (additive, `GLOW_SCALE` × the billboard, viewer-sized), and
`DISC_SCALE` stays a separate viewer knob at 2 on the billboard itself. The previous
experimental discs used half-angles 0.028/0.035 rad (3.2°/4°) and were gated on
`3.5 <= hour < 21`; both are replaced.

Two native quirks are reproduced rather than "fixed":

* both objects share the single recovered azimuth `+0x60`, so the moon is not
  placed opposite the sun;
* the disc placement uses the elevation ramps, **not** the stored
  `+0x54/+0x58/+0x5c` direction (`client_sun::sky_object_direction`, which negates
  the horizontal components and uses the day fraction as an angle). The old
  shader used that curve, which is why the discs sat in the wrong place.

The first test round reported the status line claiming the sun was switched off
at every hour, and no moon at night. Both had one cause: `Environments::endpoints`
(the non-streamed selector) took the flag word and the authored direction from
the same box it picked for fog — the nearest containing box, which is a *local*
one whenever the player stands inside one. `0x420992` reads that word from the
independently selected **global** box instead (`+0x80`; the loader maps record
`+4` to area `+0x80` at `0x48a983..0x48a98e`), so a local box carrying 0 zeroed
both fades and no disc was ever submitted. The selector now keeps the two
selections apart exactly like `RegionStreaming::endpoints`, and a world with no
containing global box keeps the recovered constructor default `0x18` rather than
borrowing a local word. `Renderer::celestial_status` also prints the latched flag
word so the state is visible in the settings tab. The latch stays native —
`client_sun::Visibility::update` stores exactly the word it is handed — and the
unknown-word fallback lives in `Renderer`, which feeds `0x18` and prints
`source 0x00 has no disc bit; viewer latches 0x18` rather than claiming the
area switched the discs off.

Unsupported states are explicit: `sky_celestial::disc` returns
`BelowHorizon { elevation_degrees }`, `Hidden { fade }`, `MissingAsset(path)` or
`NotFinite(..)`, and the World settings tab prints the outcome of both discs via
`Renderer::celestial_status`.

### 3.1 Material states (`shaders.pck`, `sun.sdr` / `moon.sdr` / `sunshine.sdr`)

All three are `version 5` Angelica effect files with the same two stages and the
same default `texturefactor 1 1 1 1`:

| Stage | Color | Alpha |
| --- | --- | --- |
| 0 | `TOP_MODULATE`, `TA_TEXTURE` × `TA_DIFFUSE` | `TOP_MODULATE`, `TA_TEXTURE` × `TA_DIFFUSE` |
| 1 | `TOP_MODULATE`, `TA_TFACTOR` × `TA_CURRENT` | `TOP_MODULATE`, `TA_TFACTOR` × `TA_CURRENT` |

with `texture TEX_NORMAL 0 Textures\nature\sun.bmp`, `...\moon.tga` and
`...\sunshine.bmp`. So each quad is texture × vertex-diffuse × texture-factor,
exactly the `fade << 24` alpha the render code packs: the disc's coverage comes
from the texture's alpha times the fade, and the texture factor provides the
per-state tint. The glow is not a separate size: `0x4b3e00` builds the sun's stage-1 factor from
the night strength, `factor = (1.0 - [+0x70]) * 5.0` (`ds:0x8d5c28` = 1.0,
`ds:0x8db0f8` = 5.0), times `0.8` (`ds:0x8d62dc`) while `[+0x74]` selects the
night set, and multiplies the `+0x44` light colour by it before submission; a
non-finite strength takes the literal `1.0` branch at `0x4b3e09`. The moon's
block (`0x4b4049`) instead halves its `+0x48` colour and scales nothing.

That rule is consumed by the shader, and the asymmetry matters. The sun's `.w`
carries `sky_celestial::glow_texture_factor / 5` and scales an additive quad
over `sunshine.bmp`; on the day set it is full strength while `+0x70` is 0 and it
falls to 0 as the night strength reaches 1, which is why the viewer's sun shows
its orange `sunshine.bmp` glow by day. The **moon's `.w` is 0**: the client
scales nothing for the moon, so no shine quad is drawn for it and
`moonflare.bmp` (decoded and bound as layer 12) is held for the flare chain of
section 3.2 instead. A moon with no halo in the viewer is therefore the expected
result of this path, not a defect: every moon halo in retail comes from that
chain, which is documented and not implemented.

No render-state call exists in the celestial render: `0x77bd00` is the only
`SetRenderState` wrapper (`vtable+0xc8`) and nothing in `0x4b3e00`–`0x4b41ee`
calls it, so the discs inherit the enclosing pass's blend state. Additive versus
alpha for `sun.bmp` therefore still rests on the asset formats, not on recovered
state.

### 3.2 The solar-flare pipeline (`CECSunMoon::Init`) — implemented in the viewer

The Sun/Moon object owns far more than two quads. Its init strings (`0x95a900`–
`0x95ac94`) enumerate: a *sun shine* stream and texture (`sunshine.bmp`), a
*flares* stream, a *flare alpha* stream, a *sun point* stream, an off-screen
*flare alpha accumulation target*, the pixel shader `shaders\ps\solarflare_14.txt`
and the textures `flares.bmp`, `sunflare.bmp`, `moonflare.bmp`.

`shaders.pck` holds that program (`ps/solarflare_14.txt`, 467 bytes): it samples
six flare-alpha textures `t0..t5`, adds their alphas with weight `0.4` each,
`saturate`s, multiplies the sum into `c0.rgb` and passes `c0.a` through — the fade
constant the render code supplies. Init also builds the streams the pass feeds
(`0x4b37d1`/`0x4b383b`/`0x4b38ac` create buffers with `0x36`, `0x5a` and `0x44`
element counts, the last through the index-buffer helper `0x784f10`), and creates
the accumulation target at `0x4b374b`–`0x4b3777` (`0x786960` on a `0x786840`
object; the descriptor carries `1, 1, 1, 0x15, 0x4d`).

**Implemented 2026-11-04.** The viewer now loads `sunflare.bmp` and `flares.bmp`
as atlas layers 13/14 and draws six flare samples at weight `0.4` (three
`sunflare` scales and three `flares` scales) in `sky_gpu::SHADER`, warm-tinted
for the sun and cool-tinted for the moon, plus `moonflare.bmp` as the moon halo
and `sunshine.bmp` as the sun streak. The sprite sizes are viewer
approximations; the client's exact flare quad sizes and the off-screen
accumulation target remain unrecovered. See
`Sky-parity-checkup-2026-11-04.md`.

---

## 4. Commands and results

```
pip install --break-system-packages capstone pefile unicorn      # 5.0.7 / 2024.8.26 / 2.1.4
objdump -d -M intel google_drive/elementclient.exe > /tmp/dis/text.asm
python3 tools/emulate-sky-sphere.py            # Unicorn run of 0x79a330 and 0x79a810
python3 tools/verify-sky-geometry.py
```

```
surround   verts  70/ 70 indices 288/288 worst component delta 7.629e-06
roof       verts 145/145 indices 792/792 worst component delta 6.676e-06

both meshes match the emulated native buffers
```

(`surround`/`roof` are the harness names for the curtain and the cap; the residual
delta is x87 extended-precision sine/cosine versus f64.)

No Rust toolchain exists in this sandbox (`cargo`/`rustc` absent, no crates.io
access), so `cargo test` was **not** executed here. The Rust unit tests encode the
same golden numbers as the Python verification above and are the first thing to run
in CI.

---

## 5. Regressions added

`src/sky_geometry.rs` — `curtain_matches_the_emulated_native_buffer`,
`cap_matches_the_emulated_native_buffer`,
`combination_covers_both_rims_without_shared_vertices`,
`island_join_is_a_duplicated_column_with_a_reversed_texture_run`,
`meridian_join_reuses_one_vertex_for_both_draws`,
`projection_agrees_with_the_vertex_buffer_inside_the_curtain`,
`projection_agrees_with_the_vertex_buffer_inside_the_cap`,
`opposite_halves_use_the_front_and_back_textures`,
`cap_edge_gap_is_reported_and_invisible`,
`curtain_alpha_fades_to_zero_at_the_recovered_rim`,
`curtain_faces_the_camera_inside_the_sphere`,
`non_finite_directions_are_rejected`.

`src/sky_time.rs` — `clock_wraps_at_four_real_hours_per_day`,
`elevations_follow_the_recovered_piecewise_curve`,
`elevation_ramps_are_continuous_apart_from_the_midnight_strength_step`,
`night_flag_is_the_texture_set_selector`,
`night_weight_is_the_linear_switch_timer`,
`frame_timer_reaches_full_weight_at_the_commit`,
`frame_timer_weight_matches_the_hour_based_weight`,
`direction_is_unit_length_and_uses_the_authored_azimuth`.

`src/sky_celestial.rs` — `disc_size_matches_the_native_quad_at_distance_100`,
`sun_and_moon_are_placed_by_their_own_elevation`,
`quad_is_a_flat_two_triangle_billboard_of_the_recovered_extent`,
`visibility_fade_is_uploaded_as_a_truncated_alpha_byte`,
`unsupported_states_fail_explicitly`.

`src/sky_gpu.rs` — the two reflection tests plus
`sky_frame_defaults_to_the_recovered_day_state`.

Time boundaries covered: 00:00, 01:30, 03:30, 12:00, 18:00, 20:00, 21:00, 24:00.

---

## 6. Remaining gaps

1. **Sky coverage above 38.16°.** The two recovered buffers leave 38.16-72.5°
   open, and retail covers it with `0x799f88`, an object that is still
   unidentified. The viewer now continues the cap's own recovered projection
   and roof-sheet texture across that band (section 1.5), which is seamless by
   construction and removes the ring/bleed of the earlier fills, but the object
   itself remains the open item — a viewer composition standing in for a
   `0x799f88` this build cannot read.
2. **Sky set identity.** Resolved: `TransSky` (`0x79ac30`) receives the three
   paths from configuration and prefixes `Textures\Sky\`; the default
   environment in `element_client.cfg` names `clear` for all three images and
   `autofamilyconfigs/sky.lst` lists the named sets (section 2.3). The viewer now
   uses that one authored set for both phases (section 2.3); the `evening` and
   `night2` folders stay loaded but are never sampled.
3. **Sky rotation.** `0x79a0e0` advances an angle at `[+0x504+0xc] * (1/30)` per
   frame and rebuilds the texture transform through `0x789ec0`/`0x789f20`
   (half-angle 0.5). The rate is set from a config/region value that is not yet
   identified, so the viewer holds rotation at 0 instead of inventing a drift.
4. **Disc blending and the flare pipeline.** `sun.sdr` / `moon.sdr` /
   `sunshine.sdr` are recovered (section 3.1) and match the viewer's texture ×
   fade alpha. The glow factor expression is consumed for the **sun only**:
   `SkyFrame.sun.w` carries `glow_texture_factor / 5` and scales an additive quad
   over `sunshine.bmp`, while `SkyFrame.moon.w` is 0 because the client's moon
   block halves its colour and scales nothing (section 3.1). The quad's **size**
   is not recovered, so it is the viewer's `sky_gpu::GLOW_SCALE` (12 × the
   1.146° billboard, ≈ 13.7° across) and the settings status says
   `+ sunshine.bmp x12 at N% (viewer size)` next to the sun and
   `no shine quad` next to the moon; the billboard itself stays at its recovered
   size. The celestial render issues no `SetRenderState` call at all (`0x77bd00`
   is the only wrapper and it is not reached from `0x4b3e00`–`0x4b41ee`), so the
   blend state is inherited and additive-versus-alpha for `sun.bmp` is still
   unproven. The whole flare pipeline of section 3.2 — three buffers, an
   off-screen alpha accumulation target, three flare textures and the `ps.1.4`
   program — is documented but not implemented, which is exactly why the retail
   moon halo is absent. Render check, 4 November 2026: the delivered build shows
   the correct sky, an orange `sunshine.bmp` glow on the sun and no halo on the
   moon, i.e. the behaviour this section predicts.
5. **Night strength consumer.** `+0x70` (up to 1.75 before 03:00) feeds the
   lighting record path (`0x79aeb0`), not the texture weights. The viewer's
   lighting is owned elsewhere and is not wired to it yet; this is also why the
   night sky set deviates (item 2).
6. **Timer units.** The 33-per-frame step is treated as 33 clock-milliseconds
   (consistent with the `1/30` fixed step at `0x8f14bc`), which makes the switch
   lengths pure functions of the hour. No x87 accumulation site proving "ms" over
   "frames" was found.
7. **Texture transform lerp.** `0x79a0e0` also interpolates the two texture
   transforms (`+0x504` → `+0x544`); the viewer has no per-set transform to
   interpolate, so only the colour/UV mapping and the set weight are applied.

---

## 7. Files

* `src/sky_geometry.rs` (new) — recovered meshes, projection, joins, coverage.
* `src/sky_time.rs` (new) — clock, elevations, night flag/strength, switch timer.
* `src/sky_celestial.rs` (new) — native disc billboard, fades, explicit failures.
* `src/sky_gpu.rs` — shader now samples the recovered projection and mixes the two
  sets with `night_weight`; crossfade and smoothstep removed; `SkyFrame` input;
  roof sheet on a repeat sampler (`GLOW_SCALE` shine layers 11/12).
* `src/client_sun.rs` — added `Visibility::fade(sun)` for per-object fades.
* `src/gpu.rs` — feeds `SkyFrame`, added `Renderer::celestial_status`, replaced the
  experimental disc label.
* `src/main.rs` — module declarations.
* `tools/emulate-sky-sphere.py`, `tools/verify-sky-geometry.py` — Unicorn harness and
  the formula cross-check quoted above.
* `tools/extract-angelic-pck.py` — reads the `configs.pck`/`shaders.pck` container
  (section 0) so the sky texture table, the celestial effect files and the flare
  shader can be re-read from the same assets.
