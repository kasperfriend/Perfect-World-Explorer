# Sky, sun/moon, flare, cloud and weather parity checkup — 2026-11-04

This is the full-sky checkup requested against the `pw142` client in the
Google Drive `pw142` folder (the `element` client and its split archives). The
`textures.zip.001` + `textures.zip.002` split reconstructs `textures.pck`
(117,047,648 bytes, 2019-05-03 build) exactly; `configs.pck` (588,068 bytes)
and `shaders.pck` (4,391,690 bytes) are the Angelica packages beside it.

## What the original client actually does

### Sky set selection is configuration, not a filename pattern

`configs/autofamilyconfigs/autoscene.cfg` (read out of `configs.pck`) says:

```ini
[Sky]
sky_id_day = 0
sky_id_night = 1
[Resource]
sky_list = Configs\AutoFamilyConfigs\sky.lst
```

`configs/autofamilyconfigs/sky.lst` has three rows, in file order:

| id | name | roof | island 01 | island 02 |
| --- | --- | --- | --- | --- |
| 0 | 晴天 (clear) | `clear\roof.bmp` | `clear\01.bmp` | `clear\02.bmp` |
| 1 | 阴天01 (overcast) | `阴天01\roof.bmp` | `阴天01\01.bmp` | `阴天01\02.bmp` |
| 2 | 夕阳 (sunset) | `夕阳\wwwww.bmp` | `夕阳\01.bmp` | `夕阳\02.bmp` |

So the night sky in this build is the **overcast** set, darkened by the
regional light record — not a painted `night2` panorama. The measured means of
the packed textures are:

| set | roof mean RGB | 01 mean RGB | 02 mean RGB |
| --- | --- | --- | --- |
| clear | 0.577, 0.680, 0.835 | 0.558, 0.643, 0.794 | 0.639, 0.709, 0.836 |
| 阴天01 (overcast) | 0.458, 0.483, 0.504 | 0.629, 0.642, 0.611 | 0.544, 0.565, 0.562 |
| 夕阳 (sunset) | 0.155, 0.169, 0.337 | 0.498, 0.422, 0.503 | 0.501, 0.424, 0.505 |
| night2 (legacy, not in sky.lst) | 0.196, 0.131, 0.213 | 0.206, 0.122, 0.200 | 0.194, 0.112, 0.187 |

The overcast set is a bright grey day sky; the client's night look comes from
the light record, exactly as the recovered `+0x70` night strength and the
regional `ambient`/`diffuse` values intend. The legacy `night2` folder is not
named by this build's `sky.lst` and is not used.

### Sun and moon: native billboard, then a flare chain

`CECSunMoon::Render` (`0x4b3e00`) submits one 4-vertex billboard per object:

* centre `direction * 100.0` (`ds:0x8d6a9c`), and the quad comes from the
  template vertex stream `CECSunMoon::Init` fills, not from a scale factor:
  `x -10..10`, `y 0..-20` for the sun and `x -20..20`, `y 0..-40` for the moon,
  so the quads subtend `2 * atan(10/100) = 11.421°` and `2 * atan(20/100) =
  22.619°`. **This corrects the "corners ±1.0, 1.1459°" reading below**; see
  `Sun-moon-flare-cloud-weather-rotation-2026-11-04.md` §1;
* gated on the object's own elevation (`+0x4c` sun, `+0x50` moon) `> 0`;
* alpha is the visibility fade (`+0x7c` sun, `+0x80` moon) in the diffuse
  alpha byte.

The retail sun/moon read larger than that quad because `CECSunMoon::Init`
(`0x4b3650`) also builds:

* a sun-shine stream over `textures/nature/sunshine.bmp` (256×128, 2:1);
* a flare stream and a flare-alpha stream (72 elements each);
* a sun-point stream (68 elements) and an index buffer (1604 indices);
* an off-screen flare-alpha accumulation target;
* the pixel shader `shaders/ps/solarflare_14.txt`, which samples six flare
  textures `t0..t5`, sums their alphas with weight `0.4`, saturates, and
  multiplies the sum into `c0.rgb`;
* the textures `textures/nature/flares.bmp` (256×256),
  `sunflare.bmp` (128×128) and `moonflare.bmp` (256×256).

The packed texture measurements are:

| texture | size | content |
| --- | --- | --- |
| `sun.bmp` | 256×256, 24-bit | radial white disc; solid core ≈55% of the quad, glow to the edge |
| `moon.tga` | 512×512, RGBA | moon disc filling ≈75% of the frame, offset in the texture, alpha bbox 11..438 × 69..493 |
| `sunshine.bmp` | 256×128, 24-bit | horizontal anamorphic streak, mean luminance 24 |
| `sunflare.bmp` | 128×128, 24-bit | radial flare sprite, mean 11.6 |
| `flares.bmp` | 256×256, 24-bit | ring/streak flare sprite, mean 17.9 |
| `moonflare.bmp` | 256×256, 24-bit | identical to `sun.bmp` (same radial profile), the moon halo |

`sun.sdr`, `moon.sdr` and `sunshine.sdr` are all `version 5` effects with
stage 0 `TOP_MODULATE(TEXTURE, DIFFUSE)` and stage 1
`TOP_MODULATE(TFACTOR, CURRENT)`. The sun's stage-1 factor is
`(1.0 - night_strength) * 5.0`, times `0.8` when the night set is selected;
the moon's block halves its colour and scales nothing. The sun's disc is
additive; the moon's is alpha-blended.

### Clouds

The world cloud surface is `world.cld` (MOXB), a grid of 36-byte vertices and
groups. `0x646450` submits the group indices as triangle lists and `0x646720`
maps the first vertex tag into a 4×4 atlas. The atlas is
`textures/cumulus01.tga` (1024×1024 RGBA, 16 cells of 256×256, white RGB with
the cloud shape in alpha). The viewer uses that texture and that 4×4 mapping.

### Weather

`CECWorld::InitNatureObjects` loads `textures/rain.tga` (32×32 RGBA) and
`textures/snow.bmp` (32×32 grayscale). The area flag word at `world.ecbsd`
record `+0x04` selects rain (bit 1) and snow (bit 2) independently
(`0x421520`). `CECWorld` advances the weather objects on a strict 33 ms
accumulator (`0x429378`). The viewer's `client_weather` clock and
`weather_gpu` selection match those rules.

## What was wrong in the viewer

1. **`configs.pck` was read with the retail v2 reader.** `configs.pck` is an
   Angelica File Package; the retail reader rejects its guard and the viewer
   silently fell back to defaults. That made `element_client.cfg`,
   `autoscene.cfg`, `sky.lst` and the AutoFamily fog unreadable. The sky
   therefore used `clear` for every slot and the night sky never changed.
2. **The night set was `night2` in the transition machine.** `Triple::NIGHT`
   was `2` (`night2`), but `sky.lst` has no `night2` row and `autoscene.cfg`
   names night `1` (overcast). The transition machine was switching to a set
   the loader never sampled.
3. **The sun and moon discs were drawn at `DISC_SCALE = 2.0` on a 1.146° quad.**
   The quad itself was wrong: the client's templates are 20 x 20 and 40 x 40
   world units at distance 100 (11.42° and 22.62°), and the sun grows by
   `1 + 2 * night_strength`. Corrected 2026-11-04 in `sky_celestial` /
   `sky_gpu`; see `Sun-moon-flare-cloud-weather-rotation-2026-11-04.md`.
4. **The flare chain was absent.** `sunshine.bmp` was drawn as a 12× shine
   quad; `moonflare.bmp`, `sunflare.bmp` and `flares.bmp` were loaded but not
   used. The moon had no halo.
5. **The AutoFamily fog fallback was dead** for the same `configs.pck` reason.
6. **`sky_pack_key` mis-prefixed `sky.lst` paths.** It turned `clear/01.bmp`
   into `textures/clear/01.bmp`, but the pack key is
   `textures/sky/clear/01.bmp`. Every `sky.lst` row therefore missed and fell
   back to the clear set. It now prefixes `textures/sky/` for `sky.lst` paths
   and `textures/` for `element_client.cfg`'s `Sky\...` paths.

## What changed

| file | change |
| --- | --- |
| `src/angelica_pck.rs` (new) | Angelica File Package reader: trailer guards `0xfdfdfeee` / `0xf00dbeef` (the shipped configs/shaders) and `0xa508bdc4` / `0x853567c8`, 280-byte trailer, backwards zlib-stream directory, 276-byte records, `pck::Entry` output. Includes a synthetic-package unit test. |
| `src/main.rs` | registers `angelica_pck`. |
| `src/environment.rs` | AutoFamily `autoscene.cfg` fog is now read with `angelica_pck` instead of the retail reader. |
| `src/sky_gpu.rs` | reads `sky.lst` + `autoscene.cfg` through `angelica_pck`; loads all three `sky.lst` sets in file order (clear, overcast, sunset) into atlas layers 0..8; exposes `day_set()`/`night_set()`; `DISC_SCALE = 1.0`; `GLOW_SCALE = 6.0`; loads `sunflare.bmp` and `flares.bmp` as layers 13/14; adds the six-sample flare chain to the sky shader; moon halo from `moonflare.bmp`. |
| `src/sky_transition.rs` | `Triple::NIGHT = 1` (overcast), `Triple::folder()` returns the `sky.lst` folders, documentation updated. |
| `src/gpu.rs` | installs the `sky_id_day`/`sky_id_night` pair into the transition machine after the sky is created; the celestial status line reports the moon halo/flare chain. |

### Flare chain in the shader

`celestial()` now draws, per object:

* the native disc at `DISC_SCALE = 1.0` (1.146°);
* the sun's `sunshine.bmp` streak at `GLOW_SCALE = 6` with the recovered
  stage-1 factor;
* the moon's `moonflare.bmp` halo at half weight (the client halves the moon
  colour and scales nothing);
* three `sunflare.bmp` samples at 2×/4×/8× and three `flares.bmp` samples at
  3×/6×/12×, each at weight `0.4`, warm-tinted for the sun and cool-tinted for
  the moon. That is the six-sample, 0.4-weight structure of
  `ps/solarflare_14`, using the client's own textures. The sprite sizes are
  viewer approximations; the client's exact flare quad sizes are not recovered.

### Night colour

The night sky is now the overcast set selected by `sky_id_night = 1`, and the
existing regional light tint (`max(ambient, diffuse) * (1 - 0.72 * night_weight)`)
darkens it through dusk. The clear set is no longer used for both phases. The
sunset set is loaded for a region that names it.

## 2026-11-06 update — the sunset line, and how long a day is

Two follow-ups after this checkup was written; the full report is
`QoL-2026-11-06-residency-time-pass-and-sunset.md`.

**The sun never reached the horizon.** The quad templates hang: `y` runs from
`0` at the emitted point to `-height` (`x -10..10, y 0..-20` for the sun, `x
-20..20, y 0..-40` for the moon, `x -60..60, y 10..-30` for the shine strip).
The emitted point `direction * 100` is the top edge, not the centre, so drawing
the quad centred on it lifted the drawing by `atan(10 / 100)` = 5.71 degrees and
the `elevation > 0` gate in `CECSunMoon::Render` (`0x4b3e00`) removed the sun
5.71 degrees above the horizon — the reported "red line above the horizon, then
the sun stops and loses its glow". `sky_celestial::template_hang` now drops the
centre by `half` along the quad's up axis (the swelled `1 + 2n` half for the
dusk sun, 20 units for the moon), and `sky_gpu` samples both the disc
(`uv = vec2(x / 2h, (y - h) / 2h) + 0.5`) and the `sunshine.bmp` strip on the
hanging, swelled rectangle, gated on that rectangle rather than on the shifted
sampling coordinate (the shift exists only to put `sunshine.bmp`'s own core, at
0.75 of its height, on the disc; gating it stretched the drawn strip into a red
tail). The elevation curve is unchanged: it is now the elevation of the setting
line, so `el == 0` is a disc that has just fully set.

**One game day is four real hours.** `CECSunMoon::Tick` (`0x4b5f10`) adds
`elapsed_ms * ds:0x8db108` to the day fraction and `ds:0x8db108` is
`6.944444444444444e-08` = 1 / 14 400 000; the `SetTime` caller `0x428f7d`
converts real time-of-day seconds with `ds:0x8d6a8c` = 6.944444e-5 = 1 / 14 400,
the same rate. So one authored hour is ten real minutes and one real second is
four authored minutes.
`clock-constants.txt` and `asm-29-clock-references.txt` hold the constants and
every code reference; the viewer's "Enable time pass" checkbox runs the clock at
that rate, with a 0.1x–600x multiplier.

## Validation and remaining gaps

* `tools/check-rust-syntax.js` passes for the new/changed files (the two
  `gpu.rs` error nodes are the documented pre-existing raw-string baseline).
  No Rust toolchain or GPU is available in this sandbox, so `cargo test` and a
  rendered frame comparison were not run here.
* The client's exact flare quad sizes, the off-screen accumulation target and
  the `0x799f88` band object are still not recovered. The flare chain uses
  viewer-chosen sprite sizes; the band fill remains the cap-projection
  composition documented in `P5-sky-geometry-daynight-celestial.md`.
* Clouds remain camera-facing billboards, not the native triangle lists; the
  atlas mapping, texture and day/night vertex colours are the client's.
* Weather remains a deterministic visual model, not the client's particle RNG
  and lifetimes; the textures, 33 ms clock and area flag selection are the
  client's.
* Sky rotation (`0x79a0e0`) is still held at zero because the rate source is
  not identified.
