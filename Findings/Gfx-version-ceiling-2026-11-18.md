# Effects after 1.4.2: the version ceiling was a measurement, not a format

**Date:** 2026-11-18
**Status:** fixed; the reader is re-based on the shipped GFX SDK source, not on
the 1.4.2 corpus maximum.

## The report

1.5.2 draws its map and its buildings but shows **no effect newer than 1.4.2**.
The logs say why:

```
gfx/…/雪.gfx would not build: GFX version 90 is newer than 79
```

Versions seen: 86, 90, 93. `effect-roots.log` recorded `previous_failures=364`.

## Root cause: three separate version refusals, all invented

Nothing in the format changes at 80–93. The number 79 came from one measurement —
the newest `MOXTVersion` in the **1.4.2** `gfx.pck` — and was then treated as the
format's ceiling in three places. The SDK settles it: the GFX source folder on
Drive (`1Zi41POuTxQXs6L_ptojvgaKD1pvuw-P3`) ships `A3DGFXEx.cpp`, which carries
the full version history **V2 → V101** and:

```c
#define GFX_CUR_VERSION 103
```

Its own `A3DGFXEx::Load` ends with `if (GFX_CUR_VERSION < m_dwVersion) return
false;` — the only version refusal in the client, and it fires at 103, not 79.

### 1. `gfx_light.rs` refused every root above 79

`if version > 79 { return Err(...) }`. That is not merely a missing glow: an
`unsupported` error marks the owner `State::Failed`, and only buildable owners
reach the player, so **the whole root's sprites and particles were dropped too**.

The gate has no basis. `A3DGFXLight::Load` takes `dwVersion` and never reads it —
fourteen unversioned lines, `LightType` through `Phi`. A 93 light record is
byte-for-byte the same shape as a 33 one. The gate is gone, and
`gfx_light_audit` (which had the same `> 79` cap) no longer caps either.

### 2. `gfx_file.rs` could not read the header of any root ≥ 84

This is the one that mattered most, and lifting the cap alone would not have
fixed it. `A3DGFXEx::Load` reads the header as a **sequence of version-gated
lines**, not as a key set:

| gate | line |
|---|---|
| ≥ 89 | `IsAngelica3: %d` — immediately after `Version`; non-zero ⇒ an Angelica-3 file this library must reject |
| ≥ 84 | `AccurateAABB: %d` — between `UseAABB` and `ShakeCam` |
| ≥ 82 | `ShakeDamp: %d` — after the shake-camera block |

The reader had **neither key**. A version-90 root was therefore refused on its
second line:

```
GFX line 2: expected DedaultScale, found IsAngelica3
```

so even after removing the light cap, every 86+ effect still failed to parse.
Both lines are now read in the client's order, gated at 89 and 84 respectively,
and `IsAngelica3: 1` is refused with the SDK's own reason. `ShakeDamp` (≥ 82) is
consumed after the shake block.

### 3. `gfx_file.rs` mis-aligned every element in a ≥ 91 root

`A3DGFXElement::LoadElementFromFile` adds two element-header blocks that the
class readers do not own:

```text
>= 91   PSFileVersion (from 95 on), ShaderFile, ShaderTex,
        PSConstCount x { PSConstIndex, PSConstValue, PSLoopCount (from 93),
                        PSTargetCount x { PSInterval, PSConstValue } }
>= 92   CanDoFadeOut
>= 81   AlphaOnly, appended to the colour-change controller (108)
```

None of these keys existed, and the parser errors on an unknown key, so a 93 root
would have failed on `PSFileVersion` as surely as on `IsAngelica3`. The shader
block is now read as its own structure (`Common::pixel_shader`), `CanDoFadeOut`
as `Common::can_fade_out`, and `AlphaOnly` as `ControlData::ColorRamp::alpha_only`
— which the player honours, keeping the RGB the element is showing and taking
only the ramp's alpha, as `A3DGFXCtrlColorChange::Process` does.

Carrying any of these below its gate is now an error rather than a skip, because
the client reads those lines *by position*: a stray extra line shifts every line
after it.

## Verification

* **The whole 1.4.2 archive still parses.** Every `.gfx` in the shipped
  `gfx.pck` (5,459 roots) was extracted and parsed: **5,457 ok, 1 failure** —
  and that one is the authored `MOXTVersion: 4` skill-record stub the reader has
  always refused by design.
* **A version-93 root plays.** Pinned by
  `gfx_player::tests::a_version_93_root_builds_a_player_and_draws_its_quads`,
  which builds a player from a 93 root and a 90 root and asserts both emit quads
  — the fault was "no effects", not "no parse", so the test is on the output.
* Header line order is pinned across every gate (24/27/38/46/50/53/63/79/82/84/
  89/90/93/103) by `gfx_file::tests::the_header_follows_the_client_line_order_at_every_gate`.
* The light element is pinned as version-invariant by
  `gfx_light::tests::a_light_record_reads_the_same_at_every_shipped_version`.

## The harness gap this exposed

`offline_harness.sh check` cannot compile `gfx_player` (it reaches `egui` through
`diagnostics`), so **nothing outside CI compiled the player**. An edit to the
colour-ramp arm — adding `alpha_only` to the enum and matching on it in
`Process` — passed `check`, `gfx` and `test` while carrying two type errors.
`offline_harness.sh player` now builds `gfx_player` with the model stack and the
real parser, stubbing only `diagnostics` and the two diag macros: 284 tests.
