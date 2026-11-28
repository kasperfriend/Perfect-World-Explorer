#!/usr/bin/env python3
"""Survey T2LM lightmap records: tile edge, tile count, and the DDS inside.

`src/terrain_lightmaps.rs` and `src/terrain_gpu.rs` both make claims about what
ships: how many tiles a file holds, what edge one tile has, and what pixel
format its DDS records use. This checks those claims against real files.

    tools/lightmap-survey.py <folder> [...]

Each `.t2lm`/`.t2lm1` is reported by name, then one combined summary states the
union the readers have to accept. Exit status is 0 even when a file is refused
-- the point is to see what exists -- but a refused file is printed as REFUSE
with the reason, never skipped silently.
"""

import collections
import os
import struct
import sys

TILES = 16
DDS_MAGIC = b"DDS "
HEADER = 24 + TILES * 4


def u32(b, off):
    return struct.unpack_from("<I", b, off)[0]


def survey(path):
    with open(path, "rb") as handle:
        b = handle.read()
    if len(b) < HEADER:
        return {"refuse": f"truncated header ({len(b)} bytes)"}
    magic, version, tiles, side, flags = u32(b, 0), u32(b, 4), u32(b, 8), u32(b, 12), u32(b, 16)
    if magic != 0x54324C4D:
        return {"refuse": f"magic 0x{magic:08x}, not T2LM"}
    if version != 2:
        return {"refuse": f"version {version}, this reader knows 2"}
    if tiles != TILES:
        return {"refuse": f"{tiles} tiles, this reader knows {TILES}"}
    if flags != 1:
        return {"refuse": f"dword 16 is {flags}, this reader knows 1"}
    tile_size = u32(b, 20)
    ranges = []
    for i in range(TILES):
        start = u32(b, 24 + i * 4)
        if start < HEADER or start + tile_size > len(b):
            return {"refuse": f"tile {i} outside payload"}
        ranges.append((start, start + tile_size))
    if any(ranges[i][0] < ranges[j][1] and ranges[j][0] < ranges[i][1]
           for i in range(TILES) for j in range(i)):
        return {"refuse": "overlapping tiles"}
    formats, edges, mip_counts = collections.Counter(), set(), set()
    for start, _end in ranges:
        if b[start:start + 4] != DDS_MAGIC:
            return {"refuse": f"tile at {start} has no DDS signature"}
        h = start + 4
        height, width = u32(b, h + 8), u32(b, h + 12)
        mips = u32(b, h + 24)
        fourcc = struct.pack("<I", u32(b, h + 80)) if u32(b, h + 76) & 0x4 else None
        formats[fourcc if fourcc else f"raw{u32(b, h + 84)}"] += 1
        edges.add((width, height))
        mip_counts.add(mips)
    return {
        "side": side,
        "tile_size": tile_size,
        "edges": edges,
        "mips": mip_counts,
        "formats": formats,
    }


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    total = 0
    refused = 0
    sides = collections.Counter()
    edges = collections.Counter()
    mips = collections.Counter()
    formats = collections.Counter()
    for root in argv[1:]:
        if not os.path.isdir(root):
            print(f"skip {root}: not a directory", file=sys.stderr)
            continue
        for dirpath, _dirnames, filenames in os.walk(root):
            for name in sorted(filenames):
                if not name.lower().endswith((".t2lm", ".t2lm1")):
                    continue
                path = os.path.join(dirpath, name)
                total += 1
                result = survey(path)
                if "refuse" in result:
                    refused += 1
                    print(f"REFUSE {name:28} {result['refuse']}")
                    continue
                hdr = "128" if result["side"] == 128 else "256"
                print(
                    f"ok     {name:28} header edge {hdr:>3}  tile {result['tile_size']:>6} B  "
                    f"dds {sorted(result['edges'])}  mips {sorted(result['mips'])}  "
                    f"{dict(result['formats'])}"
                )
                sides[result["side"]] += 1
                edges.update(result["edges"])
                mips.update(result["mips"])
                formats.update(result["formats"])
    print()
    print(f"{total} file(s), {refused} refused")
    if sides:
        print("  header tile edge : " + ", ".join(f"{k} (x{v})" for k, v in sorted(sides.items())))
    if edges:
        print("  DDS dimensions   : " + ", ".join(f"{w}x{h} (x{c})" for (w, h), c in sorted(edges.items())))
    if mips:
        print("  DDS mip counts   : " + ", ".join(f"{k} (x{v})" for k, v in sorted(mips.items())))
    if formats:
        print("  DDS formats      : " + ", ".join(f"{k} (x{v})" for k, v in sorted(formats.items())))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
