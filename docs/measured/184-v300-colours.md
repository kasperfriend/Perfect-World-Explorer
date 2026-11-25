# 1.8.4 BMD v300 colour and tail audit

Date: not verified in this checkout.

Status: **loader control flow measured; no 1.8.4 BMD fixture or corpus validation.** The `0x10000300..=0x10000305` path in `src/bmd.rs` is provisional, not a client-verified schema. This note records what the executable does establish and what it does not.

## Artifact identity and source boundary

Drive metadata and file headers verify the source identities that were previously cross-attributed in the notes:

| Artifact | Drive ID | Verified identity / size |
| --- | --- | --- |
| `A3DLitModel.cpp` | `1s-G7Xu_158Hg4_rC7Ff8atyrU7djv6QN` | `A3DLitModel.cpp`, 66,759 bytes; workspace copy SHA-256 `fae5197332774e52a3694635ec3ce4e1dc54188cb3462d231e0eea13f52ec386` |
| `A3DMesh.cpp` | `1xcZ3lzP_c-8TIhP7koeuuUrS2QlbGST1` | `A3DMesh.cpp`, 48,366 bytes |
| `A3DLitModel.h` | `17jsMpX2QpNHqa4nkXTQ0M6BIL00DlF0H` | `A3DLitModel.h`, 12,235 bytes; text extraction succeeded |
| `A3DMaterial.cpp` | `1aEkHgFjGJRK3C-_bSzkQodrNgFO4vaKH` | `A3DMaterial.cpp`, 8,878 bytes |
| `AFile.cpp` | `1gzif7CHUXyb012bkqXi9_75x_1Lrycqt` | `AFile.cpp`, 5,785 bytes |

The source headers date these engine files to 2001–2004. They are useful corroboration for older Angelica serialization, **not** source for the 2023 client’s `0x100003xx` parser. The header declares `A3DLITMODEL_VERSION = 0x10000002`, `A3DLITMODEL_LIGHTMAP_VERSION = 0x10000101`, `A3DLITMESH_VERSION = 0x10000006`, and `A3DLITMESH_SUPPORT_LIGHTMAP_VERSION = 0x10000100`. Its `A3DLitModel::Load` handles model values `0x10000001`, `0x10000002`, `0x10000100`, and `0x10000101`; its save path emits only `0x10000002` or `0x10000101`. Those values do not explain the 1.8.4 dispatch below.

The client examined here is `/tmp/google_drive_staging/elementclient.exe`, PE32 i386, SHA-256 `9195638821ef3fc7411072307143d8d76253db31c3bb7bbc6095efdeb6c10408` (the staged binary’s linker timestamp is 2023-11-21). The address references below are virtual addresses in that image’s `.text` disassembly.

## Version dispatch: do not call these versions interchangeable

At `0x00f4c92c`, the model loader compares the version word as follows:

| Version word | Client route | Evidence |
| --- | --- | --- |
| `0x01000030` | `0x00f4d6f8` | `cmp eax,0x1000030`; equality branch at `0x00f4c931` |
| `0x10000301` | `0x00f4d6f8` | comparison at `0x00f4c937`; equality branch at `0x00f4c93c` |
| `0x10000302` | different parser, beginning `0x00f4c94d` | comparison at `0x00f4c942`; equality falls through at `0x00f4c947` |
| `0x10000300`, `0x10000303..=0x10000305` | not present in this dispatch | No matching immediate was found in the examined model-loader dispatch / `.text` scan |

In particular, `0x01000030` is **not** `0x10000300`; the shared jump for the first two rows does not establish either aliasing or a common layout for the entire `0x10000300..=0x10000305` range. The current `is_v300` range in `src/bmd.rs` is an implementation assumption, not a recovered client version table.

## What the shared `0x00f4d6f8` branch reads

The shared branch begins with four 12-byte reads into model fields, then a 4-byte mesh count, then two distinct one-byte reads (`0x00f4d781`, `0x00f4d79a`). Those bytes are consumed before the mesh loop. The second byte selects two different mesh-loading paths; the first controls a later model-level string/map path. Their names and complete semantics are not established here.

In the ordinary file-backed path (the branch selected when the second model byte is zero), the disassembly shows this sequence:

1. Two string objects are read, followed by two 32-bit counts (`0x00f4dab6..0x00f4db1b`). The Drive `AFile.cpp` exposes a length-prefixed `ReadString(AString&)` overload; do not confuse it with its separate NUL-terminated `ReadString(char*, ...)` overload.
2. The loader reads `nVerts * 24` bytes of vertex records (`0x00f4db44..0x00f4db7d`). The copy loop uses the first 12 bytes as position, preserves a 4-byte diffuse value and two 4-byte UVs, and fills normals from a separate `nVerts * 12` block.
3. It reads `nFaces * 6` bytes of input indices (`0x00f4db85..0x00f4dbc0`): three 16-bit indices per face, widened into the runtime index buffer by the following loop. This is not the 12-byte-per-face `u32` input assumed by the current provisional parser.
4. It calls the material loader at `0x00f4dcec` before the optional per-mesh blocks. `A3DMaterial::Load` in the source and the client body at `0x00f50b00` agree on the binary record: a NUL-terminated `MATERIAL: <name>` string, then ambient, diffuse, emissive and specular values (16 bytes each), a 4-byte power, and a 1-byte two-sided flag (69 bytes after the string).
5. The first per-mesh byte read at `0x00f4d86f` is passed into mesh initialization and gates the later `nVerts * 8` block (`0x00f4dcff..0x00f4dd1c`). The two flag bytes jointly control the pair of `nVerts * 4` arrays (`0x00f4dd30..0x00f4dd86`): both arrays are read if either flag is zero, and both are skipped only when both flags are nonzero. The exact user-facing names/meaning of the second byte and the arrays remain open; **the client chooses these blocks from flags, not by examining color alpha bytes**.
6. A 48-byte AABB read occurs per mesh (`0x00f4dd8c..0x00f4dda1`). Thus, in this branch, material loading precedes the optional arrays and AABB.

The alternate path selected by the second model byte uses a descriptor/table route (`0x00f4d8b3..0x00f4dab1`) and is not the same byte stream path. It also handles indices as 16-bit input values before expanding them. Full field semantics for that route remain open.

## Model-level tail is after the mesh loop

After the mesh loop, an optional model-level string/map path is selected by the first model byte (`0x00f4dffc..0x00f4e095`). The loader then reads one 32-bit length at `0x00f4e0a1`. If the value is greater than four, it reads exactly `length - 4` bytes. It copies the first 16 payload bytes to model fields, then parses per-mesh 16-byte records only when the remaining payload length is exactly `16 * mesh_count` (`0x00f4e17c..0x00f4e216`). Otherwise it skips interpreting those records, but has still consumed the length-delimited block. This is a model-level tail, after all meshes; its record semantics are not identified here.

## Audit of the current Rust v300 path

`src/bmd.rs` currently accepts the whole `0x10000300..=0x10000305` range, although the examined client dispatch only shows routes for `0x01000030`/`0x10000301` and `0x10000302`. For `0x10000303..=0x10000305`, the parser also assumes a 4-byte word before the mesh count. After the count it assumes two model-level bytes; the first is ignored and a nonzero second byte takes an unsupported shared-data error path. For each mesh it assumes two flag bytes, length-prefixed GBK name and texture strings, counts, 24-byte vertices and a normal block. It records the per-mesh flag bytes but does **not** use them to choose its colour-block branch, and the disassembly does not establish this complete byte layout from a file fixture.

The index reader first tries 32-bit values if the candidate block fits and every value is below the vertex count, then falls back to 16-bit values. That is a heuristic, not verified offset logic; the observed ordinary client route reads 16-bit input indices. Colour/UV classification is likewise inferred from an exact empty-name `MATERIAL: \\0` tag at selected offsets, block-size checks, and a 90%-opaque test of the first or following candidate blocks; some cases substitute `0xff808080`. The tail code probes for a 48-byte AABB, the same exact material tag and a size field, then uses fixed 70- or 150-byte fallbacks and may scan up to 256 bytes for something that resembles the next mesh header. None of these advances has been validated against a raw 1.8.4 BMD, so the parser's classifications and resynchronization logic are not a format specification.

### Fallback-color mesh/reason audit boundary

No real mesh can be named from the evidence currently available: the 1.8.4 `litmodels.pck`/`.pkx`/`.pkx1` archive and a raw BMD fixture are unavailable in this checkout. The only fallback *reason* that can be stated is the current Rust heuristic, not a verified retail reason: for a nonempty mesh, it sets both day and night diffuse to `0xff808080` when fewer than 90% of the first candidate block's day-alpha bytes are `0xff` **and** fewer than 90% of the following candidate pair's corresponding day/night words both have alpha `0xff`. That following pair is peeked only when the parser believes `8 * vertex_count` bytes remain. If the first candidate's day-alpha count reaches 90%, the parser keeps the first pair as colours; it does not require the night-alpha count to pass or copy day into night. Other heuristics can skip an `n * 8` candidate block when an exact material tag occurs after `n * 16` bytes, or replace the initial pair with the following pair when the latter passes the joint-alpha threshold. These are parser branches, not measured 1.8.4 mesh classes, and cannot identify affected retail mesh names or counts without BMD bytes.

That is **not equivalent** to the observed 1.8.4 route: the shared client path consumes two model bytes before entering the mesh loop; its ordinary mesh path reads 16-bit input indices; color/lightmap blocks are flag-controlled and occur after material load; the per-mesh AABB follows those blocks; and a separate size-prefixed extension follows the whole mesh list. The material-name reader also accepts `MATERIAL: <name>\0`, not only the empty-name byte sequence `MATERIAL: \0` used by the parser's `MAT_TAG` probes.

Consequently, the alpha-threshold classification and neutral-colour fallback are parser heuristics, not client-measured behaviour. In this checkout, `tools/offline_harness.sh objects` passed 101 tests, including all 19 `bmd::tests`; the BMD fixtures are synthetic or negative cases. The separate `tools/offline_harness.sh test` run passed 234 tests but does not include `src/bmd.rs`. These tests prove only that the parser handles its own constructed buffers and named refusals; they do not validate a retail 1.8.4 BMD. No version range, flag meaning, colour branch or next-mesh offset should be promoted to a client-verified claim from them.

## Remaining blocker and next evidence

No real 1.8.4 BMD was read. The previously measured `world.ecwld` (Drive ID `1-JLS8LwSur1r_l5_8AD9XY4yvPTPqM76`) and paired `world.ecbsd` are placement/scene files, not BMD payloads; they provide no per-mesh flags or next-mesh offsets. Their earlier tree/effect census is in [`184-world-tree-effect-audit.md`](184-world-tree-effect-audit.md) and was not rerun in this checkout. The earlier `/tmp/litmodels.pck.zip` fragment contained only the `litmodels.pck` piece and could not enumerate the logical archive; the matching `.pkx` and `.pkx1` data remain unavailable in the recorded evidence. The inventory and failed large-file probes remain in [`184-drive-inventory.md`](184-drive-inventory.md) and [`184-litmodels-tail.md`](184-litmodels-tail.md).

Still-open audit outputs, stated rather than inferred:

- **Archive branch counts and verified next-mesh offsets:** none can be reported without the logical 1.8.4 archive and raw BMD members. The loader branches are disassembled, but no fixture validates their per-mesh advances.
- **Fallback-color mesh/reason list:** the code-branch conditions above are known, but no real mesh name, mesh index, fallback count or retail reason is known.
- **Render-check summary:** no BMD render check was run. There is no fixture to render and no paired native reference frame; parser/census results are not visual evidence.

To close this safely: obtain at least one raw 1.8.4 BMD for each observed loader route, record the version word, model/per-mesh flag values and every next-mesh offset, confirm the color/lightmap block choice against those flags, and validate the model extension size/records. Then implement the exact `0x01000030`/`0x10000301` shared route separately from `0x10000302`; only expand accepted versions when the client dispatch and a real fixture support them. Until then, P3.01 is reopened and this parser branch remains provisional.
