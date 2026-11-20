# T2MK layer, mask and world-to-texture survey (P2.06)

Archive survey, not one area. Regenerate with
`tools/terrain-layer-survey.py --dir <dir with the .t2mk files> <out.json>` for
the whole archive, or `tools/terrain-layer-survey.py <file.t2mk> --csv <out.csv>`
for the per-entry rows. The machine-readable result of the archive pass is
[Phase-2-terrain-archive-survey.json](Phase-2-terrain-archive-survey.json); the
per-entry rows for the reference tile are in
[Phase-2-terrain-layer-survey.csv](Phase-2-terrain-layer-survey.csv).

Reference client `elementclient.exe` 1.4.2, md5
`5f02895688fa580506e869156340a664`.

## 1. Covered

| Item | Value |
|---|---|
| Files surveyed | **88** `world_1.t2mk` … `world_88.t2mk`, 79,716,831 bytes (76.0 MiB) - the complete `world_<n>` set shipped in the archive, contiguous, none missing |
| Files rejected by the reader | 0 |
| Version | 2 in every file (magic `0x544d4b42`; version cap 2 at `0x7a15c5`) |
| Grid dimension byte (v2, `0x7a1662` -> `env+0x891`) | 1 in 88/88 files |
| Areas | 16 per file, **1,408** area records in total |
| Layer records | **603** in total; 1…18 per file; values seen: 1,2,3,4,5,6,7,8,9,10,11,12,14,15,16,17,18 |
| Area-layer entries | **5,929** (one base + 4,521 masked) |
| Masks | **4,521**, all 256x256, all zlib-compressed, each decoding to exactly 65,536 bytes: 282.6 MiB uncompressed |
| Base entries | 1,408, all with `mask_size = 0` (a base never has a mask) |
| Masked entries | 4,521, all `mask_size = 256`; no other resolution occurs |
| Record `flags` (`+0x04`) | `0xffffffff` in all 603 records |
| Axis bytes | 0: 39 records, 1: 527, 2: 37 - all three occur, none outside 0/1/2 |
| Areas by stack depth | 1: 251, 2: 99, 3: 134, 4: 128, 5: 218, 6: 570, 7: 8 |
| Distinct texture indices | 162 (0…161); 559 references total, 1…18 distinct per file |
| Authored uv scale pairs | 31 distinct, of which 24 square and 7 non-square |

Stack-depth histogram is over area records, so 251 single-layer areas plus 1,157
multi-layer areas add up to the 1,408 areas above.

### Tiles with no terrain at all

Eight files are exactly 368 bytes and contain `16 areas, 1 layer record, 0
entries`: `world_73`, `world_74`, `world_75`, `world_76`, `world_81`,
`world_82`, `world_83`, `world_85`. The client's own loader accepts them (they
are v2 with the same header), so the survey parses and reports them instead of
treating them as corruption. They are the only source of the "1 layer record"
figure. Because they contribute no entries, dropping them would not change any
stack, mask or axis count.

### Coverage statement

88 of 88 `world_<n>.t2mk` files, 1,408 of 1,408 of their areas and 5,929 of
5,929 of their area-layer entries. What is **not** covered: the other terrain
sets in the archive (`login_login`, the `a*`, `b*`, `s*` dungeon/instance tiles),
which are separate worlds with their own `TRN2` and are not part of the shipped
1.4.2 mainland. The survey tool accepts those files unchanged; they were simply
not downloaded, and no claim is made about them here.

## 2. Layout

```
+0x00  u32  magic 'BKMT' (0x544d4b42)
+0x04  u32  version (<= 2; 0x7a15c5)
+0x08  u32  area count                       ; 16 in every file
+0x0c  u32  layer count                      ; 1..18
+0x10  u32  grid dimension byte (v2 only, 0x7a1662 -> env+0x891)
+0x14  u32[area] offsets to per-area lists
        layer table follows the offset array: 28 bytes per record (0x7a178c)
        +0x00 u32 texture index
        +0x04 u32 flags (0xffffffff in every surveyed record)
        +0x08 u32 mask size (0 = layer has no mask)
        +0x0c u8  axis, +0x0d..+0x0f padding
        +0x10 f32 u scale
        +0x14 f32 v scale
        +0x18 u32 layer id (authoring/order tag, 0..4)
per-area list: u32 count (<= 100, 0x7a1676), then count * { u32 layer, u32 offset, u32 size }
```

Every masked entry's `size` (`+0x08` of the list entry) is the compressed byte
length of its zlib stream; the uncompressed length is always `mask_size²`.

## 3. Ordering and record sharing

`area_layer` is the stack order the client draws. The first entry is the base
and has no mask; each later entry is mixed over the accumulated result with its
own mask, so a stack of 7 is one base plus six masked layers.

Facts from the survey that constrain the ordering rule:

| Observation | Count |
|---|---|
| Areas whose stored order is already non-decreasing in record `+0x18` (the tag) | 1,343 of 1,408 |
| Areas where one tag value repeats | 1,065 of 1,408 |
| Areas whose stored order is non-decreasing in the layer-record **index** | 1,408 of 1,408 |
| Areas that use the same record twice | 0 |
| Files whose areas share records (fewer records than entries) | 80 of 88 |

So draw order is the file order of the per-area list, and the record index is
the unique key for a layer inside a file (`world_60`: 6 records, 88 entries).
The `+0x18` tag is **not** a draw index - it repeats inside a stack, and sorting
by it would reorder 65 areas, which is why `src/material.rs` and
`src/terrain_gpu.rs` both build the stack from the list order.

## 4. Axes and world-to-texture mapping

| Axis byte | Layer records | Area base layers | Masked layers |
|---|---|---|---|
| 0 | 39 | 0 | 384 |
| 1 | 527 | 1,408 | 3,766 |
| 2 | 37 | 0 | 371 |

Every one of the 1,408 area **base** layers has axis 1. Axis 0 and 2 occur only
on masked layers, and 755 of the 4,521 masked layers use an axis different from
their own area's base, so the projection is genuinely per layer and cannot be
hoisted to the area.

The authored uv scales confirm that: 1,193 of 4,521 masked layers reuse their
base's scale and the rest do not, and 7 of the 31 scale pairs are non-square
(`u != v`), e.g. `(0.005, 0.003333)` 37 times and `(0.04, 0.045455)`.

`axis` is the record byte at `+0x0c`. Three readers consume it - the WGSL in
`src/terrain_gpu.rs`, the CPU sampler in `src/material.rs`, and the mesh emitter
in `src/gpu.rs` - and **they agree**. The earlier note in this section claimed
the first two disagreed for axis 1; that was wrong, and the arithmetic shows why:

`material.rs` reads a grid cell `(gx, gy)` plus a height `h` and forms
`world_x = (gx - w/2)*s`, `world_z = (h/2 - gy)*s`, then maps
`0 -> (world_z, -h)`, `2 -> (world_x, -h)`, else `(world_x, -world_z)`. Its `-h`
*is* the WGSL's `-world.y`: the height `h` is the Y coordinate of the vertex at
that cell, which `gpu.rs` builds as `pos = [(x - w/2)*s, height, (h/2 - y)*s]`.
Substituting gives `0 -> (z, -y)`, `1 -> (x, -z)`, `2 -> (x, -y)` in both, and
`material.rs`'s catch-all arm is exactly the axis-1 case written as the default.

So the axis contract is settled and consistent across the viewer, and axis 1 -
the axis **every one of the 1,408 area bases uses** - is `(world.x, -world.z)`.

What is *not* recovered is the **native** projection: whether the client's
per-vertex texcoords use that same mapping, mirror, and scale. The vertex shader
`terrain_vs` writes `oT0..oT5` from pre-built texcoords, so the projection
happens on the CPU mesh path, and no address in the surveyed range emits it.

## 5. Lightmap pairing

`world_39.t2lm` (read via `src/terrain_lightmaps.rs`, reader `0x7a18c0`): magic
`0x54324c4d`, version 2, 16 tiles of 256x256, 16 distinct offsets, 32,896 bytes
per tile. Tile choice is per area, from the same counter that selects the mask
slot (`0x7cd760`), not from world position.

## 6. Unsupported / rejected

The survey raises `Unsupported`, naming the file (and the area and layer for
per-entry problems), and never guesses:

| Case | Behaviour |
|---|---|
| `.t2mk` version > 2 | rejected (`0x7a15c5`); names the file |
| per-area layer count > 100 | rejected (`0x7a1676`); names the file and area |
| non-base entry with `mask_size == 0` | rejected; names file, area and layer |
| mask that does not decode to `size*size` | rejected; names file, area and layer |
| grid-dimension byte other than 0/1 | rejected; names the file |
| mask size other than 256 | no instance in 88 files; the reader accepts any `<= 8192` and the code path above 256 is untested |
| axis byte other than 0/1/2 | no instance; no native default recovered |
| the `a*`/`b*`/`s*`/`login` terrain sets | not downloaded, not surveyed |
