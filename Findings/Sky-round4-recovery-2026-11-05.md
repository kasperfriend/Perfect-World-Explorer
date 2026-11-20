# Sky round 4 — the flare metric, the drift matrix and the sky-set chain

Everything below is read out of `elementclient.exe` (1.4.2, `0x400000` image) and
the shipped `configs.pck` / `textures.pck` dumps in `Findings/recovery/`. No
value here is inferred from artwork.

## 1. The ceiling is drawn through a stage-0 texture matrix

* `A3DSky::Tick` `0x79a040` calls `A3DSky::Rotate` twice per frame:
  * `0x79a054` `lea ecx,[esi+0x504]; push &[esi+0x4cc]; call 0x79a0e0` — the
    committed ceiling matrix, built from the descriptor at `+0x4cc`.
  * `0x79a077` `lea eax,[esi+0x544]; push &[esi+0x4e8]; call 0x79a0e0` — the
    incoming matrix used only while `+0x5b4` (the swap timer) is running.
* `A3DSky::Render` `0x799a40` enables the transform for texture stage 0
  (`0x77a1f0(0, 2)` at `0x799a8d`, i.e. `D3DTTFF_COUNT2`) and installs the
  matrix with `0x779da0(0, matrix)` at `0x799ab1`. `0x779da0` is
  `A3DDevice::SetTextureMatrix`: it caches eight matrices at
  `this+0xcf4+stage*0x40` and calls the D3D device's `SetTransform` with
  `D3DTS_TEXTURE0 + stage`; its failure string is
  `D3DDevice SetTextureMatrix Failure`.
* The curtain `0x799b40` installs nothing of its own: it inherits whatever the
  last `0x779da0` left, and in the render sequence
  (`0x799f30`-`0x799f83`, `Findings/recovery/asm-05-sky-band-object.txt`) the
  matrix set before the two curtain draws is stage **1** from the identity
  block, so the panorama keeps the authored UVs. Only the ceiling is
  transformed.

### The descriptor (`this+0x4cc`, seven floats)

`A3DSkySphere`'s constructor `0x7995e0` (`Findings/recovery/asm-11-sky-ctor-init.txt`)
writes:

| slot | value | meaning in `A3DSky::Rotate` |
| --- | --- | --- |
| `+0x00` | `0x41000000` = **8.0** | diagonal scale (`m00 = m11 = v`, `m22 = 1`) |
| `+0x04` | `0x3c9374bc` = 0.017989 | rate, accumulates into `+0x10` |
| `+0x08` | `0x3c9374bc` = 0.017989 | rate, accumulates into `+0x14` |
| `+0x0c` | 0 | time; the angle only advances when this is non-zero |
| `+0x10` | 0 | accumulated x |
| `+0x14` | 0 | accumulated z |
| `+0x18` | 0 | angle |

`CECWorld::LoadSky` `0x42a8e0` then overwrites the two rate slots with
`0x3b449ba6` = **0.003** (`0x42a98d` `mov [esi+0x4d0],eax` / `mov [esi+0x4d4],eax`)
and leaves everything else alone. `A3DSkySphere::TransSky` `0x79aeb0` can copy a
whole seven-float descriptor from its caller (`rep movsd ecx,7` at `0x79afa6`
into `+0x4cc`, at `0x79b042` into `+0x4e8`), but the caller in `0x4203f4` passes
`NULL`, so the shipped values are the constructor's.

### The matrix `A3DSky::Rotate` `0x79a0e0` builds

1. If `[+0x0c]` (time) is non-zero: `angle += time * 0.0333333` (one 30 Hz
   tick, `0x8f14bc`), then `base = Rotate(angle)` about `(-0.5, -0.5)`
   (`0x789ec0`, `0x789e90`). Time is zero in the shipped descriptor, so this
   whole branch is skipped — **there is no rotation**.
2. If the scale slot is not exactly 1.0: `base = Scale(v, v) * base`
   (`0x787170` multiply; the scale matrix is `m00 = m11 = v`, `m22 = 1`).
3. If either rate is non-zero: `accum += rate * 0.0333333` per call, then
   `base = Translate(accum_x, accum_z) * base` (`0x789f20`).
4. The result is a 3x3 (nine floats, stride `0x10` in the copy) which is what
   `SetTextureMatrix` takes.

So the ceiling UV arrives at the sampler as `8 * uv + drift`, with the drift
growing `0.003 / 30` per 30 Hz tick in both axes — `0.003` texture units per
second, i.e. one full sheet per 5.5 minutes. The cap's own UVs span the whole
sheet (`0.5 + p.xz / 30.070580`), so the 8x scale means the sheet repeats up to
eight times from the zenith to the horizon.

The viewer implements exactly this: `SKY_DRIFT_SCALE = 8.0`,
`SKY_DRIFT_RATE = 0.003`, the accumulator advances by
`rate * (logic_delta_ms / 1000)` in `begin_frame` (identical to `rate` per
second at 30 Hz and framerate independent elsewhere), and the cap samples
`cap_uv(...) * p.skyrot.w + p.skyrot.xy` through the repeat sampler. The
curtain is untouched. The unit test
`the_ceiling_runs_through_the_recovered_texture_matrix` pins all of it.

### The cap's lookup, corrected

`0x79aa73` divides by `R * sin(+0x4c4)` = 15.035290 and halves the sum
(`(1 + p.x / 15.035290) / 2`), so the stored vertex UVs are
`0.5 + p.xz / (2 R sin(step))` = `0.5 + p.xz / 30.070580`: the whole texture
spans the cap, its centre overhead and its rim on the horizon. Because `Render`
drops the object by `cos(step) * R` = 47.685848, the eye sits that far above the
sphere's centre, and a ray of elevation `dy` reaches the sheet at
`t = -47.685848*dy + sqrt(2500 - 2273.9399*(1 - dy*dy))` (15.035290 at the
horizon, 2.314152 overhead). The lookup is therefore `0.5 + t * d.xz / 30.070580`.

The previous viewer used `0.5 - 0.5*sin(acos(0.95371695*dy) - asin(dy))`, which
is right only at the horizon and collapses the whole sky above 45 degrees onto
the sheet's centre; that radial collapse is what read as a dome. The corrected
lookup is exact against the vertex buffer:
`projection_agrees_with_the_vertex_buffer_inside_the_cap` casts the ray from the
dropped eye and reproduces every stored UV to 1e-4.

## 2. The flare sprite metric is the viewport height

`0x4b6270` (the fifteen-quad flare stream builder) reads its host block once,
copies six dwords from `arg1+0x14` to `[esp+0x280]`, and then:

```
004b679f: mov edx, dword ptr [esp + 0x290]        ; fifth copied dword
004b67ae: mov dword ptr [esp + 0x5c], edx
004b67b2: mov dword ptr [esp + 0x1c], 0
004b67ba: fild qword ptr [esp + 0x5c]             ; read as an INTEGER
004b67be: fmul dword ptr [esp + ebp + 0x194]      ; * size[i]
004b67c5: fst dword ptr [esp + 0x4c]
004b67c9: fstp dword ptr [esp + 0x24]             ; = the sprite's half extent
```

The six-dword block is `{?x, ?, width, height, ?, ?}` starting at `+0x14`, so the
fifth dword (`arg1+0x24`) is the viewport **height**. The block comes from
`0x4360a0`, a one-instruction getter (`mov eax,[ecx+4]; ret`) called at
`0x4b5b51` with the caller's own host object, so the metric is a host-owned
integer.

Half extent is therefore `height_pixels * size[i]`, which in NDC-height units is
exactly `size[i]` — the value the viewer already passes as the `metric`
argument of `flare_chain`. Sizes, offsets and UV origins stay the builder's
tables (`Findings/recovery/asm-01c-flare-stream-builder.txt`).

## 3. The sky-set chain, end to end

1. `configs/element_client.cfg` `[DefEnv]` gives the environment defaults:
   `sky_image01 = Sky\clear\Roof.bmp`, `sky_image02 = Sky\clear\01.bmp`,
   `sky_image03 = Sky\clear\02.bmp` (read by `CECConfigs::LoadConfigs`
   `0x40cc30`, which pushes `0x94e978` / `0x94e960` / `0x94e948`).
2. `configs/autofamilyconfigs/autoscene.cfg` `[Sky]` picks two rows of the list:
   `sky_id_day = 0`, `sky_id_night = 1`.
3. `configs/autofamilyconfigs/sky.lst` is six lines per row — count, then
   `id`, `label`, `roof`, `01`, `02`, one legacy flag:

   ```
   3
   0 晴天    clear\roof.bmp    clear\01.bmp    clear\02.bmp    0
   1 阴天    阴天01\roof.bmp   阴天01\01.bmp   阴天01\02.bmp   0
   2 夕阳    夕阳\wwwww.bmp    夕阳\01.bmp     夕阳\02.bmp     0
   ```

   (each on its own line in the real file). `crate::sky_config::parse_sky_list`
   already reads precisely this layout.
4. `0x4203f4` in the scene tick drives the swap: `[ebp+0x100/0x104/0x108]` is
   the day triple, `[ebp+0x10c/0x110/0x114]` the night triple, the weight is
   `[[esi+4]+0x50]+0x70`, and the night flag is `+0x74`. Weight 0 uses the day
   triple, weight 1 the night triple; anything in between crossfades, with the
   fade length from `0x4b6250` (`0x1b7740` = 30 min day / `0x249f00` = 40 min
   night) times the weight and 1.25, or times `(1 - weight) * 2` on the way in.
5. `A3DSky::SetSky` `0x79ac30` prefixes `Textures\Sky\` to the three names
   (`0x98c0e8`) and returns 1 (meaning "differs, needs a load") when the names do
   not match the ones already on the three texture slots `+0x5a4/+0x5a8/+0x5ac`;
   `A3DSkySphere::TransSky` `0x79aeb0` then loads them
   (`0x77c1d0(dest, "Textures\Sky", name, 4, 1)`), releasing the previous
   textures through `0x77c560`.

Render order (`0x799f30`-`0x799f83`) uses them as: `+0x5ac` = the cap
(`clear\roof.bmp`), `+0x5a4`/`+0x5a8` = the two curtain halves
(`clear\01.bmp`, `clear\02.bmp`).

## 4. Tideborn: there is no zone-to-sky mapping in this build

* `textures.pck` ships 68 sky folders (226 files), including `夜晚01`…
  `夜晚04`, `海盗岛夜晚`, `雪山夜晚天空`, `night2` and `a满堂天空\<area>\…`.
* None of those names appears anywhere in `configs.pck` (dumped text) or in the
  client's sky strings (`sky_id_night` at `0x97b768` is the only night-sky
  string in the image).
* `configs.pck` carries exactly one `autoscene.cfg` and one `sky.lst`, and
  `sky_image01..03` exist only in `element_client.cfg`.
* The world is a single scene (`autoscene.cfg` `scene_path = cache\EditMaps\Scene00`,
  `border_file = …\borders\scene00.border`), so one autoscene.cfg governs it.

A starry night sky in a particular zone therefore cannot come from this build's
shipped configuration: it would need a per-map `AutoScene.cfg`/`sky.lst` that
selects a different row (or `sky_image` override) — the client would read it
through the same `0x6c12c0` parser, but no such file ships in `configs.pck`.
The viewer already resolves any row that exists, so a client that does ship one
would light up without code changes if it replaces `configs/autofamilyconfigs/`.

What the shipped data does say, and what the viewer now shows: day = `clear`,
night = `阴天01` (overcast), both from `sky.lst`.
