# Native verification of the Phase 1 claims (fresh, 2 November 2026)

This file records a **fresh static analysis** of the retail client supplied by the
repository owner. It is not a transcription of earlier research-log entries; the
addresses below were re-checked against the binary in this session.

## Material examined

| | |
|---|---|
| File | `elementclient.exe` |
| Source | owner's Google Drive, `Games/pw142/element/` (file id `19h1s6U7JR0uvVccqeL8ZTAVpVeal1X1X`) |
| Size | 6,000,640 bytes |
| MD5 | `5f02895688fa580506e869156340a664` |
| SHA-256 | `a290998dc13cc5558b54bda3f65d099…` |
| PE | i386, image base `0x400000`, `SizeOfImage` `0x5f7000`, timestamp `0x4B1F1D57` (2019-05-03) |
| Sections | `.text` `0x401000`+`0x4d3f25`, `.rdata`, `.data`, `.tls`, `.data1`, `.rsrc` |
| Tool | `capstone` 5.0.7 + `pefile` 2024.8.26, linear sweep of `.text` |

Payloads: `maps/world/world_8.t2lm` (id `1nkjKuDER6DSO9nstHuS-Ybs2XESN8wtB`) and
`world_8.t2lm1` (id `1rHg2gloVOrmoZyakRL5HGc8MP7yAIOk3`), 526,424 bytes each.

The sweep decoded **1,730,446 instructions** and **139,666 distinct literal xref
targets**. Literal operands are reliable here because this is a fixed-base,
non-PIE MSVC build: absolute addresses appear directly in `call`, `mov` and `[disp]`
forms. Indirect calls through tables are *not* indexed, which is the one systematic
blind spot of this method.

## 1. Sky-object curve: who consumes it (P1.05)

| Claim | Result |
|---|---|
| `0x4b60b0` computes the azimuth | Confirmed: `fld [eax+8]; fchs; fld [eax]; fchs; fpatan; fstp [ecx+0x60]` — i.e. `atan2(-z, -x)`, the exact expression in `client_sun.rs` |
| `0x4b60b0` has one caller | **Confirmed: exactly 1**, at `0x41f2e6`, inside the regional-environment function `0x41e756` |
| `0x4b60e0` is the elevation + day/night clock | Confirmed: reads `qword [ecx+0x68]` (a double), writes `[ecx+0x4c]` (sun), `[ecx+0x50]` (moon), `[ecx+0x70]` (night factor), `[ecx+0x74]` (moon phase) |
| `0x4b60e0` callers | **Exactly 2**: `0x4b5f5e` and `0x4b607b`, both inside the sky-object function `0x4b3656` |

### Verified constants (`.rdata`, float32 unless noted)

| Address | Value | Role | `client_sun.rs` counterpart |
|---|---|---|---|
| `0x8db13c` | `0.145833328` | 3.5 h / 24 | `0x3e155555` |
| `0x8db138` | `6.85714293` | 60° ramp slope | `0x40db6db7` |
| `0x8db134` | `1.04719758` | 60° | `0x3f860a92` |
| `0x8d5894` | `0.5` | noon | `0.5` |
| `0x8db130` | `3.44959211` | rise slope | `0x405cc61e` |
| `0x8db12c` | `0.875` | 21 h / 24 | `0.875` |
| `0x8db128` | `2.66666675` | descent slope | `0x402aaaab` |
| `0x8db124` | `1.22173047` | 70° | `0x3f9c61aa` |
| `0x8db120` | `8.37758064` | night slope | `0x41060a92` |
| `0x8d6018` | `0.125` | 3 h / 24 | daylight ramp start |
| `0x8db11c` | `0.291666657` | 7 h / 24 | daylight ramp end |
| `0x8db118` | `6.0` | ramp slope | `daylight()` |
| `0x8db110` | `0.75` (f64) | 18 h / 24 | sunset start |

**Conclusion.** The recovered curve is consumed by the sky object only. Material
light is *not* derived from it; see below.

## 2. Who installs material light (P1.04)

The light-install function `0x79b390` copies 9 dwords into `[ecx+0x10]` (a
D3DLIGHT-style struct on the renderer, `ecx = [[0x9b3eec]+0xc]` = device wrapper).
It has **16 call sites in 8 functions**:

| Call sites | Function | What it installs |
|---|---|---|
| `0x41f29b`, `0x41fdb1`, `0x420db0` | `0x41e756` (regional environment) | the environment's directional light — on init, on key change, and per frame |
| `0x4303a3` | `0x430330` | wrapper forwarding a caller-supplied light |
| `0x650418`, `0x6505fc` | `0x64ffe0` (grass) | grass-local light |
| `0x6bf738` | `0x6bf230` | renderer-scoped light |
| `0x792c20`, `0x792d65`, `0x792d7b` | `0x7924f0` (skin/model) | per-model point light; `0x792d7b` pushes `0x42480000` = **50.0** as the range |
| `0x7a69b9`, `0x7a6caa`, `0x7a7e36` | `0x7a6426`, `0x7a73e0` (BMD) | ordinary model material light from `[edx+0xd8]` |
| `0x7ab9b9` | `0x7ab6a0` | model light |
| `0x7d28e6`, `0x7d2921` | `0x7d23a0` | object light from `[edi+0x50]`/`[edi+0x6c]` |

So the *environment* direction is produced by `0x41e756` alone, and the entities
that receive it are the ones enumerated in `light_consumers::MATRIX`. Both the
init (`0x41f29b`) and the key-change (`0x41fdb1`) installs are the sites already
named in the report, now confirmed by direct disassembly.

## 3. Terrain/grass lightmap payloads (P1.06)

`world_8.t2lm` and `world_8.t2lm1`, parsed independently:

```
magic 0x54324c4d ("T2LM"), version 2, 16 tiles, tile side 256, DDS flags 0x1
tile size 32896 = 128-byte DDS header + 256*256/2 (DXT1, 1 mip)
offsets 88, 32984, 65880, ... (stride exactly 32896), no overlap, span == file length
each tile begins "DDS " ... height 0x100, width 0x100
day and night differ in all 16 tiles; the 16 day tiles are all distinct
```

This is exactly what `terrain_lightmaps::Tiles::parse` and the DDS gate in
`terrain_gpu::load_lightmaps` require (`format == Dxt1`, `mips.len() == 1`,
`256x256`), so the reader's assumptions are validated against retail payloads
rather than only synthetic fixtures. It also confirms that authored night maps
really do exist per area, which is the premise of the availability split.

## 4. Lightmap binding path (P1.06)

`0x7c2650` (`ApplyLightMap`) has **4 call sites, all inside one function,
`0x7c2270`** (`0x7c23a9`, `0x7c23b5`, `0x7c2507`, `0x7c2513`) — i.e. the client
binds a lightmap while setting up the block it is about to draw, and has no
separate loading/eviction manager. The preceding block at `0x7c2623..0x7c263f`
stages a constant into the material state and calls `0x779da0` before returning,
which is the "no map bound" arm.

Consequence for the viewer: an eager atlas with one slot per mask area produces
the same *bound sampling state* as the client's per-block bind. The difference is
resource management (the viewer keeps every slot resident), not observable
sampling behaviour. This is recorded as a deviation, not as missing behaviour.

## 5. Ambient and visibility paths (P1.04)

* `0x420e77` calls `0x799540` with `lea ecx,[esp+0x50]` and then `0x77a450` with
  `or eax,0xff000000` — the packed-ambient store exactly as described.
* `0x420992..0x4209bf` reads the **default** area's `+0x80` word twice:
  `shr 3; and 1` → `[edx+0x84]` (sun) and `shr 4; and 1` → `[edx+0x85]` (moon).
  The two bits are published independently of the local box that supplies
  colour/fog, confirming the ownership rule the viewer now encodes.

## 6. Retail BMD corpus (P1.07)

Both world model packs were reassembled from the owner's split archives and
decoded end to end with an independent parser written for this check.

| | `litmodels.pck` | `building.pck` |
|---|---|---|
| Source | `litmodels.zip.001..005` | `building.zip.001..004` |
| Size | 397,425,471 B (zips total 390,080,669 B) | 326,185,591 B |
| MD5 | `359be12bc2aab785210ab82506a67aaf` | `d23d8941ed15f42153477f1c083af50f` |
| Content | **28,858 `.bmd` files** | textures only (dds/tga/bmp) |

`litmodels.pck` decoded with **zero parse failures** (PCK v2, `0x20002`, guard
keys `0xfdfdfeee`/`0xf00dbeef`, 28,858 entries):

* 68,362 meshes: **44,376 v4** (`0x10000004`) and **23,986 v5** (`0x10000005`).
* Every mesh has a texture; no mesh has an empty texture name.
* **2,579 of the 23,986 v5 meshes are two-sided**, 21,407 single-sided.
* 59,984 of 68,362 meshes have `day != night` in their two baked arrays, so the
  integer blend is live for 88% of the content and both arrays must be kept.
* **Every v5 material is the identity:** computed name `"MATERIAL: "`, ambient
  `(1,1,1,1)`, diffuse `(1,1,1,1)`, emissive `(0,0,0,1)`, specular
  `(0,0,0,1)`, power `0`. Only `two_sided` varies. There are therefore **no
  client-specific colour or light overrides in the retail world BMD pack** — the
  appearance comes from the texture and the baked vertex arrays alone. (An
  earlier scan counter in this session counted the emissive *alpha* channel and
  wrongly reported non-zero emissive; the values above are the corrected ones.)

### Texture-stage sequence

`0x7a9730` is the world-BMD material applier. The D3D wrappers it calls push a
fixed D3DTSS type, so the sequence decodes unambiguously:

| Call site | Wrapper | Meaning |
|---|---|---|
| `0x7a976d` | `0x779fc0` (COLOROP) | stage 0 COLOROP = **5 (MODULATE2X)** |
| `0x7a977a` | `0x779fe0` (COLORARG1) | stage 0 colour arg = texture |
| `0x7a9785` | `0x77a020` (ALPHAOP) | stage 0 ALPHAOP = **4 (MODULATE)** |
| `0x7a9792` | `0x77a040` (ALPHAARG1) | stage 0 alpha arg |
| `0x7a979e` | `0x779fc0` | stage 1 COLOROP = **1 (DISABLE)** |
| `0x7a97aa` | `0x77a020` | stage 1 ALPHAOP = 1 (DISABLE) |

One applier, one stage configuration, and it matches the production shader
(`raw_tex.rgb * i.diffuse * 2.` with a texture-alpha cutout). Baked world models
are therefore drawn in the single opaque queue with a cutout test — there is no
transparent baked queue whose ordering would need reproducing.

## 7. Not verified here
* The texture-stage switch inside `0x7a8d90` (MODULATE2X at `0x7a9730`) was
  located, not enumerated case by case; P1.07 therefore keeps its open items.
* `models.pck` (888 MB, character/effect BMDs) was not decoded; the corpus
  above is the world BMD pack, which is what the phase-2/3 BMD path renders.
* The alpha-test reference (`0x7a59c5..0x7a5b6f`) and the `0x497423fe` distance
  cap belong to the *model* queue, not to baked world BMDs, and were not
  re-derived here.
* Indirect calls (vtables, function pointers) are not in the xref index, so
  "exactly N callers" always means "exactly N literal call sites".
