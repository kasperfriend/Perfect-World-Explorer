# Terrain shader variants and pass state (P2.04)

Recovered from `elementclient.exe` 1.4.2, md5 `5f02895688fa580506e869156340a664`,
6,000,640 bytes, PE i386, image base `0x400000`. Encoded as
`src/terrain_shading.rs`; exercised by its `#[cfg(test)]` tests through
`tools/check-phase1-core.sh`.

## 1. Variant selection

`A3DTerrain2Render::ApplyMask` at `0x7c1450` is the only selector. It is called
from exactly three sites, all in the terrain submitter: `0x77df92`, `0x77e064`,
`0x77e160`.

| Test order | Native | Condition | Target | Client name |
|---|---|---|---|---|
| 1 | `0x7c1455` | `[0x9d30fc]+0xc == 0` | `0x7c1e00` | no pixel pipeline |
| 2 | `0x7c1465` | `env+0x1b5 != 0` | `0x7c1a20` | per-mask lightmap |
| 3 | `0x7c1469` | `env+0x1b6 != 0` | `0x7c2100` | hardware lightmap |
| 4 | `0x7c1482` | otherwise | `0x7c14b0` | 3-layer batch |

`env` is `[render+0x4]`; `[0x9d30fc]` is the `A3DTerrain2Env` singleton
(`0x7c46c0` lazy init, `0x7c47d0` constructor).

```text
007c1450: mov 0x9d30fc,%eax
007c1455: mov 0xc(%eax),%dl
007c1458: test %dl,%dl
007c145a: je  0x7c14a0
007c145c: mov 0x4(%ecx),%eax
007c145f: mov 0x1b5(%eax),%dl
007c1465: test %dl,%dl
007c1467: jne 0x7c1491
007c1469: mov 0x1b6(%eax),%dl
007c146f: test %dl,%dl
007c1471: je  0x7c1482
007c1478: call 0x7c2100
007c1487: call 0x7c14b0
007c1496: call 0x7c1a20
007c14a5: call 0x7c1e00
```

`0x7c170a` re-reads the same `env+0x19c` byte inside `0x7c16a0`; `0x7c26ee`
re-reads `env+0x1b6` inside `0x7c26e0`. Neither is an independent flag.

### Reachability

| Variant | Reachable | Evidence |
|---|---|---|
| 3-layer batch | **yes**, the stock path | `env+0x1b5` has two writers on this object: `0x77d0e4` (zero, in the terrain-env constructor `0x77ce50`) and `0x77d4d8`, `mov %al,0x1b5(%esi)` with `%al = ([[0x9d30fc]+0xc] != 0) ? <byte at 0x20(%esp) of 0x77d1d0> : 1`; `env+0x1b6`'s only writer is `0x77d0ea`, `mov %bl,0x1b6(%esi)`, and `xor %ebx,%ebx` dominates it, so it is always 0 |
| per-mask lightmap | reachable whenever `env+0x1b5` is set | same writer. **Not a debug key:** the strings `PerMask` and `Terrain2PerMask` do not occur in `elementclient.exe`, so the earlier label is retracted. Which value the shipped game supplies is unrecovered, so "reachable", not "default", is the honest word |
| no pixel pipeline | only when the terrain pixel shaders fail to load | `0x7c1455` reads `[[0x9d30fc]+0xc]`, which `0x7c48b7` sets from the `0x7c4d90` `Configs\terrain2_shaders.cfg` load; it is not a device capability query |
| hardware lightmap | **never** | gate `env+0x1b6` cannot be set; the path also clears both shader slots (`0x7c21ac` -> `0x77baf0`/`0x77bad0`) and then uploads `c0..` pixel constants at `0x7c22c2` |

`select()` returns `Err(VariantError::HardwareLightmapUnreachable)` instead of
silently falling back. The matching row for the shared
`Findings/Unsupported-client-paths.md` register is proposed (not applied) in
[the report](Phase-2-Agent-B-report.md), section 7.

## 2. Shader modules

Loaded in `0x7c4d90` from `Configs\terrain2_shaders.cfg` (`0x98f9cc`) via
`0x76c730`, gated by `Configs\terrain2ps.cfg` section `general`/`binary`
(`0x98dbac`/`0x98dbb4`, read at `0x7c4df6`).

| Handle | String | Address | Shader |
|---|---|---|---|
| `env+0x1c` | `Shaders\ps\t2_3layer_batch_14.txt` | `0x98f954` | `ps.1.4`, 3 layers + packed mask + day/night lightmap |
| `env+0x20` | `Shaders\ps\t2_3layer_vertlit_14.txt` | `0x98f930` | `ps.1.4`, 3 layers, no lightmap |
| `env+0x24` | `Shaders\ps\t2_tex_merger.txt` | `0x98f910` | `ps.1.4`, one mask per layer |
| `env+0x28` | `Shaders\ps\t2_tm_lm.txt` | `0x98f8f8` | fixed-function day/night merge |
| `env+0x2c` | `Shaders\vs\terrain_vs.txt` | `0x98f8dc` | shared terrain vertex shader |
| `env+0x30` | `Shaders\vs\terrain_refract_vs.txt` | `0x98f878` | water-refraction vertex shader |

`0x7c236d` binds `env+0x1c` before every layer batch, but the stride is taken
from `env+0x1b5` (`0x7c0d99`), so the flag swaps the **vertex layout**, not the
pixel module. The bundle still contains `t2_3layer_vertlit_14` and `t2_tm_lm`
even though no terrain path in the terrain range binds them; that is a
contained observation, not a claim that another subsystem never does.

## 3. Pass state

`0x7cd760` (per area) brackets the draw:

| State | Wrapper | Value | Addresses |
|---|---|---|---|
| `CULLMODE` 22 | `0x77a540` | 1 (`D3DCULL_NONE`); `env+0x168` is the state cache slot the wrapper stores the requested enum in, not a saved previous value | set `0x7cd789`, popped `0x7cda6c` |
| `ZWRITEENABLE` 14 | `0x77a230` | 1 | `0x7cd793`, restore `0x7cda4b` |
| `ZENABLE` 7 | `0x77a210` | 1 | `0x7cd79d`, restore `0x7cda55` |
| `LIGHTING` 137 | `0x77a3c0` | 1 in the bracket (`0x7cd7c1`); 0 for the draw and back to 1 after, but **only** on the per-mask (`0x7c1a65`/`0x7c1ae8`) and no-pixel-pipeline (`0x7c1e15`/`0x7c1ef0`) paths - the stock 3-layer path (`0x7c14f5`/`0x7c1578`) leaves it at 1 | `0x7cd7c1`, `0x7c14f5`, `0x7c1a65`, `0x7c1e15` |
| `SPECULARENABLE` 26 | `0x77b570` | 1 | `0x7cd7c9` |
| `0x8e` / `0x8f` | `0x77bd00` | 1 / 1 (shade mode; diffuse+specular) | `0x7cd7da`, `0x7cd7e9`, in `0x7cda41` `0x8f = 0` after the draw |
| `ALPHABLENDENABLE` 27 | `0x77a290` | **never called** | implemented at `0x77a2af`, no caller in `0x7c1400-0x7d3fff` |
| `ALPHATESTENABLE` 15 | `0x77a320` | **never called** | implemented at `0x77a335`, no caller |
| `SRCBLEND` 19 | `0x77a2c0` | 2 then 5 (3-layer), 5/5 (fixed function) | `0x7c1509`, `0x7c158c`, `0x7c1e29`, `0x7c2141` |
| `DESTBLEND` 20 | `0x77a2f0` | 6 (`D3DBLEND_INVSRCALPHA`) | `0x7c1513`, `0x7c1596`, `0x7c1e33`, `0x7c214b` |

Blend factors are installed per batch but `ALPHABLENDENABLE` is never enabled in
the bracket, so terrain is opaque with depth writes. Depth function is not set
by terrain: `ZFUNC` (23) has exactly one wrapper (`0x77a270`, `0x77a27e`) and no
terrain caller.

## 4. Texture stages

| Stage | 3-layer batch | per-mask | no pixel pipeline | hardware |
|---|---|---|---|---|
| `t0..t2` | 3 layer colours | 2 colours + merger stages | modulate2x (`0x7c1e3f`) | `0x7c210b`, `0x7c2219`, `0x7c2225` |
| `t3` | packed mask | mask `n` | mask (`0x7c1f32`) | — |
| `t4` / `t5` | day / night lightmap | — | disabled | lightmap |

`D3DTSS_TEXCOORDINDEX` (11) maps stage -> texcoord set with `(stage, stage)`
(`0x7c2231`, `0x7c2239`, `0x7c2245`); the area draw overrides stage 2 with
`0x20000` at `0x7cd937` = index 0 + `D3DTSS_TCI_CAMERASPACE`, the projected
lightmap coordinate. `D3DTSS_TEXTURETRANSFORMFLAGS` (24) = 0 for plain stages and
`0x104` for the projected one (`0x7cd946`). `mov oT3/oT4/oT5, v5` in both vertex
shaders is why the mask stage asks for texcoord 5.

`D3DTSS_COLOROP` = 5 (`D3DTOP_MODULATE2X`) on stage 0 of the fixed-function path
and 4 (`D3DTOP_MODULATE`) on the mask stage: the fixed-function pipeline doubles
the composite exactly like `mul_x2` does in `t2_3layer_batch_14`.

## 5. Vertex layout

`0x7c0d99`: `sete %al` on `env+0x1b5 == 0`, `lea 0x34(,%eax,4),%eax`.

| `env+0x1b5` | FVF | Stride | Layout |
|---|---|---|---|
| 0 | `0x412` | `0x38` | `XYZ` (12) + `NORMAL` (12) + `TEX4` (32) |
| != 0 | `0x4c2` | `0x34` | `XYZ` (12) + `DIFFUSE` (4) + `SPECULAR` (4) + `TEX4` (32) |

The declaration `0x7c4e78` binds `v0` position, `v1` normal, `v2..v5` four
`float2` texcoords, matching `terrain_vs`.

## 6. Unsupported

| Case | Behaviour |
|---|---|
| hardware lightmap (`env+0x1b6`) | `VariantError::HardwareLightmapUnreachable`; row proposed for `Unsupported-client-paths.md` |
| per-mask variant | state table complete, texture arithmetic not yet implemented in the viewer (shader `t2_tex_merger`, stage loop `0x7c25c0`) |
| layer axis other than 1 | every surveyed area *base* layer is axis 1; axes 0 and 2 occur only on masked layers, and their world-to-texture projection is unrecovered (see `Phase-2-terrain-layer-survey.md` §4) |
| no-pixel-pipeline variant | state table complete; the viewer always selects the 3-layer batch because `Caps::default()` reports a device pixel pipeline |
| `t2_3layer_vertlit_14`, `t2_tm_lm` | loaded by the client, not bound by any terrain path in `0x7c1400-0x7d3fff` |
