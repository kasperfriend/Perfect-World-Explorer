#!/usr/bin/env python3
"""Survey the DDS pixel formats a pack actually ships (item 74 of PART 3).

`src/model_dds.rs` decides whether it can read a surface by matching the
`DDS_PIXELFORMAT` FourCC. That match was `DXT1` / `DXT3` / `DXT5`, and the
question this tool answers is which of those, and which others, the staged
archives really contain -- because a format the reader accepts and no pack
carries is dead code that reads like coverage, and a format a pack carries and
the reader refuses is a member lost with nothing saying so.

Usage:
    tools/dds-survey.py PACK [PACK ...] [--json OUT] [--list-unreadable]

Exit status is 0 even when a format is unreadable; this is a survey, not a gate.
"""
import argparse
import collections
import json
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pck_lib as pck  # noqa: E402

# Every `Format` variant `src/model_dds.rs` can decode. A FourCC outside this
# set is refused by name there, so it is a real gap and not a silent skip.
READ = {"DXT1", "DXT3", "DXT5"}

# DDS_HEADER field offsets, from the header `model_dds.rs` validates: magic,
# `dwSize` 124, `DDS_PIXELFORMAT` at 76 (`dwSize` 32, `dwFlags` +4, fourCC +8).
PF_FLAGS = 80
FOURCC = 84
HEIGHT = 12
WIDTH = 16
MIPMAPS = 28
RGB_BITS = 88
# `DDPF_FOURCC` / `DDPF_RGB` / `DDPF_LUMINANCE`.
DDPF_FOURCC = 0x4
DDPF_RGB = 0x40
DDPF_LUMINANCE = 0x80000

# The uncompressed layouts `src/model_dds.rs` decodes, spelled the way
# `classify` reports them. A layout absent from here is refused by name there,
# so it is a real gap and not a silent skip -- and this list has to be checked
# against the reader, not guessed from the pack:
#
#   32-bit 0xff0000/0xff00/0xff        BGRA or BGRX
#   16-bit 0x7c00/0x3e0/0x1f           RGB565
#   24-bit 0xff0000/0xff00/0xff        BGR, no alpha byte
#
# 24-bit was in this pack set (33 members) and the reader already accepts it;
# listing it here is what keeps the survey from reporting readable members as
# unreadable.
READ_RGB = {
    "32-bit 00ff0000,0000ff00,000000ff,ff000000",
    "32-bit 00ff0000,0000ff00,000000ff,00000000",
    "16-bit 00007c00,000003e0,0000001f,00000000",
    "24-bit 00ff0000,0000ff00,000000ff,00000000",
}

# `DDPF_LUMINANCE`: one channel widened into R, G and B. 8 and 16 bits, both
# carried by the staged `textures.pck` caustic set.
READ_LUMINANCE = {8, 16}


def classify(data):
    """Return `(kind, detail)` for one DDS payload, or `(None, reason)` if it is
    not a DDS at all."""
    if len(data) < 128 or data[:4] != b"DDS ":
        return None, f"{len(data)}-byte payload is not a DDS"
    pf = struct.unpack_from("<I", data, PF_FLAGS)[0]
    if pf & DDPF_FOURCC:
        tag = data[FOURCC:FOURCC + 4].decode("latin1").rstrip("\0")
        return ("fourcc", tag)
    if pf & DDPF_LUMINANCE:
        bits = struct.unpack_from("<I", data, RGB_BITS)[0]
        return ("luminance", f"{bits}-bit")
    if pf & DDPF_RGB:
        bits = struct.unpack_from("<I", data, RGB_BITS)[0]
        masks = struct.unpack_from("<IIII", data, RGB_BITS + 4)
        return ("rgb", f"{bits}-bit {','.join(f'{m:08x}' for m in masks)}")
    return ("other", f"pixel-format flags {pf:#x}")


def survey(path):
    version, entries = pck.directory(path)
    rows = []
    for name, offset, size, _flag in entries:
        if not (name.lower().endswith(".dds") or name.lower().endswith(".dds2")):
            continue
        data = pck.read_entry(path, (name, offset, size, _flag)) if offset else b""
        kind, detail = classify(data)
        row = {
            "name": name,
            "kind": kind,
            "detail": detail or "",
            "readable": (kind == "fourcc" and detail in READ)
            or (kind == "rgb" and detail in READ_RGB)
            or (
                kind == "luminance"
                and int(detail.split("-")[0]) in READ_LUMINANCE
            ),
        }
        if kind is not None:
            row["width"] = struct.unpack_from("<I", data, WIDTH)[0]
            row["height"] = struct.unpack_from("<I", data, HEIGHT)[0]
            row["mips"] = struct.unpack_from("<I", data, MIPMAPS)[0]
        rows.append(row)
    return version, rows


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("packs", nargs="+")
    ap.add_argument("--json")
    ap.add_argument("--list-unreadable", action="store_true")
    args = ap.parse_args(argv)

    all_rows = []
    for pack in args.packs:
        try:
            version, rows = survey(pack)
        except pck.EncryptedPackage as e:
            # A sealed pack is not a failed survey; see `pck_lib`.
            print(f"SKIPPED: {pack}: {e}")
            continue
        print(f"{pack}  version {version:#x}  dds members {len(rows)}")
        all_rows.extend(rows)

    counts = collections.Counter(
        (r["kind"], r["detail"]) for r in all_rows if r["kind"] is not None
    )
    print()
    print(f"  {'kind':8s} {'format':34s} {'count':>7s}  {'read':>5s}")
    for (kind, detail), n in counts.most_common():
        readable = (
            (kind == "fourcc" and detail in READ)
            or (kind == "rgb" and detail in READ_RGB)
            or (kind == "luminance" and int(detail.split("-")[0]) in READ_LUMINANCE)
        )
        print(f"  {kind:8s} {detail:34s} {n:7d}  {'yes' if readable else 'NO':>5s}")

    bad = [r for r in all_rows if not r["readable"]]
    print()
    print(f"  dds members surveyed: {len(all_rows)}")
    print(f"  unreadable:           {len(bad)}")
    if bad:
        shown = bad if args.list_unreadable else bad[:20]
        for r in shown:
            print(f"    {r['name']}  ({r['kind']} {r['detail']})")
        if len(bad) > len(shown):
            print(f"    ... and {len(bad) - len(shown)} more; use --list-unreadable")

    # The point of the table: name the formats the reader accepts that no pack
    # in this run carries, so dead support is visible instead of implied.
    seen = {d for (k, d) in counts if k == "fourcc"}
    unused = sorted(READ - seen)
    if unused:
        print()
        print(f"  accepted by src/model_dds.rs but carried by no pack here: {', '.join(unused)}")

    if args.json:
        Path(args.json).write_text(
            json.dumps({"rows": all_rows}, ensure_ascii=False, indent=1),
            encoding="utf-8",
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
