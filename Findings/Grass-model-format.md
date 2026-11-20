# Grass model format (`.mox` / MOXB) - settled

P4.05 carried "no `.mox` byte layout" as an open item, and earlier work tried to
recover the vertex stride by fitting candidate strides against a byte budget.
That fitting failed for every combination and the failure was misleading. The
layout is instead read straight out of the parser the viewer already ships and
then confirmed against the whole retail archive.

Source archive: `grasses.pck`, 2019-05-03 build (the one whose `shaders.pck`
matches this client's), 1,701 members, 155 of them `.mox`.

## Layout

```
"MOXB"
"VERSION: 2\0"            optional
"FRAME: \0"
u32  frameCount           must be 1
[u8; 64]                  basis, identity in every archive model
u32  childCount           must be 0 in every archive model
u32  meshCount
per mesh:
  "MESH: \0"
  u32 vertexCount
  u32 indexCount          (a multiple of 3)
  u32 frameCount          must be 1
  [u8; 10]                padding
  u16 index[indexCount]
  vertex[vertexCount]     32 bytes each:
                            f32 pos[3] @0, f32 normal[3] @12, f32 uv[2] @24
  "TEXTURE: \0" <GBK name> "\0"
  "MATERIAL: \0" ...
```

The vertex/index block is anchored *backwards* from the `TEXTURE:` marker:
`indices_start = texture_offset - vertexCount*32 - indexCount*2`. That is why
the stride has to be known to parse at all, and why fitting it by budget alone
was ambiguous.

## Why the earlier attempt failed

The mesh header is three consecutive `u32` (vertex count, index count,
per-mesh frame count). It was previously read as `u16,u16,u16`. For
`水草01.mox` the real values are `18, 36, 1`; read as `u16` that becomes
`18, 0, 36`, which silently shifts every downstream offset by one byte and
makes all 691 remaining bytes look like they fit no stride. The archive member
is 905 bytes total, so a 32-byte stride was the answer all along.

## Corpus confirmation

Running the shipped parser over every `.mox` in the archive:

| | |
|---|---|
| models parsed | 155 / 155 |
| vertices examined | 4,703 |
| non-finite coordinates/normals/uvs | 0 |
| normals with \|n\| != 1 (2% tol) | **0** |
| out-of-range indices | 0 |

4,703 consecutive unit-length normals landing on a 32-byte cadence is not a
coincidence that a wrong stride can produce; a one- or four-byte error would
desynchronise within the first few vertices. The uv values are ordinary
corners and 11% of them fall outside [0,1], which is normal for grass cards
that tile or mirror their texture, not evidence of misalignment.

A concrete member is now a unit test: `grasses/做好的花/荷叶01.mox` (409 bytes,
the smallest model) is a 2x2 quad with all normals `(0,1,0)`, uvs
`(0,1) (1,1) (0,0) (1,0)` and a 2.0 x 2.0 footprint.

## Still open under P4.05

The format is settled; what remains is placement and LOD, not parsing:
`cam[+0x678]` turns out to be a lerped colour lane rather than a depth, so the
viewer still runs the density ladder at step 1, and patch visibility/streaming
is not reproduced.
