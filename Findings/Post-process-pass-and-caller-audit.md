# Post-process pass and caller audit (P1.08)

Scope: close the P1.08 gate, which was stated as "final whole-pass/caller
coverage audit; not another missing basic bloom implementation".

Sources: `google_drive/elementclient.exe` (1.4.2) and `google_drive/shaders.pck`
(2019-05-03 set, v0x20002). Every claim below is a string or shader read out of
those two files, not an inference from behaviour.

## The chain the client actually builds

The whole post chain lives in one class, `CECFullGlowRender`. Its
`LoadResource` error strings enumerate every resource it creates, in order:

| order | string address | resource |
|---:|---|---|
| 1 | `0x94ed3c` | sharpen stream |
| 2 | `0x94ed80` | `fullsharpen_14.txt` |
| 3 | `0x94ede8` | bump map for underwater |
| 4 | `0x94ee38` | `spacewarp_tl.txt` |
| 5 | `0x94ee98` | `spacewarp.txt` |
| 6 | `0x94eef4` | warp stream index buffer |
| 7 | `0x94ef44` | warp stream |
| 8 | `0x94ef88` | blur y stream |
| 9 | `0x94efcc` | blur x stream |
| 10 | `0x94f010` | stretch copy stream |
| 11 | `0x94f05c` | `monofullglow.txt` |
| 12 | `0x94f0c0` | `fullglow2.txt` |
| 13 | `0x94f11c` | `fullglow1.txt` |
| 14 | `0x94f178` | glow render target 2 |
| 15 | `0x94f1c4` | glow render target 1 |
| 16 | `0x94f210` | glow back target |
| 17 | `0x94f258` | requires an A8R8G8B8 device target |

Two things this settles:

* **`spacewarp_tl.txt` is a load-time alternative, not a pass.** It sits between
  the underwater bump map and `spacewarp.txt` in the same loader, so it is the
  second source for the same effect (the `_tl` suffix marks the transform-and-
  lighting / fixed-function variant used when the vertex-shader path is
  unavailable). Comparing the two shaders confirms it: both use
  `def c1, -1, 1, 0, 1` and `def c2, 0.05, 0.05, 0, 1`, both do the same
  `mul`/`mul`/`mad` offset and the same `sub r0.r, r0.a, c2.r` + `texkill`.
  The only differences are `texcrd r1.xy` (projected) versus `r1.xyz` and the
  `mov r1.z, c1.a` that only the projected form needs. Their output is
  identical, so implementing either one covers the effect.
* **`c2.r = 0.05` is the texkill threshold, not the warp offset.** The offset
  scale arrives per-vertex (`r2` is a vertex texcoord) from a stream the client
  builds at run time. The viewer previously treated the whole `0.05` as an
  offset coefficient and omitted the `texkill`; the texkill is now reproduced
  in `src/post_gpu.rs`. The per-vertex offset scale is still approximated,
  because the warp stream's vertex data is built in code the client never
  serialises.

## Shaders referenced but outside the main frame

| shader | owner (adjacent error string) | belongs to P1.08? |
|---|---|---|
| `solarflare_14.txt` | `CECSunMoon::Init` | No - celestial sprite, P5.03 |
| `shadowrenderblur_14.txt` | `CECShadowRender::Init` | No - shadow map blur, separate pass |
| `dropshadow_14.txt` | (font/text sprite) | No - UI text |

These three are loaded by other classes and are not part of the
`CECFullGlowRender` main-frame chain, so their absence is not a P1.08 gap.

## Per-pass coverage

Every program in the `CECFullGlowRender` chain is present in the pack and driven
by the viewer (`src/post_gpu.rs`, `src/post_plan.rs`):

* `fullglow1_14` - horizontal blur accumulate.
* `fullglow2_14` - vertical blur, `lrp(original, blur, glow level)` plus the
  `glow power * blur^2` term.
* `monofullglow_14` - the mono-hue variant, `def c4, 0.35, 0.6, 0.25, 1.0`.
* `fullsharpen_14` - four-tap sharpen with `def c4, -1,-1,-1,1`; note each
  `mad` reuses the *original* `r5`, which the viewer implements.
* `spacewarp` / `spacewarp_tl` - underwater warp, texkill now included.
* `dropshadow_14`, `shadowrenderblur_14`, `solarflare_14` - owned elsewhere,
  listed above.

## Verdict

The gate asked for a coverage audit, and it is done: the chain has 17 named
resources, all of them accounted for, and the one shader that was previously
unimplemented from the chain (`spacewarp_tl`) is now shown to be a duplicate of
the one the viewer already runs. The texkill was a real omission and is fixed.

What this does **not** establish: the exact per-vertex offset scale of the warp
stream, and the glow target dimensions beyond the already-recovered
quarter-size rule. Both remain recorded here rather than claimed.
