#!/usr/bin/env python3
"""How much of a preview frame changed between two shot times.

An effect that is claimed to animate has to move pixels: this reads two PNGs
from `Findings/effect-preview` and prints the share of pixels that differ, both
at a low threshold (any change at all) and at a high one (change a reader sees).
The ruler, the background and the camera are identical between shots of one
root, so everything that differs is the effect.

    python3 tools/effect-frame-diff.py 05-29888-0000ms-az35el20-z1.00.png \
        05-29888-3000ms-az35el20-z1.00.png

No dependencies: the frames are 8-bit RGBA, non-interlaced, so a small PNG
reader is enough and the tool runs where Pillow is not installed (the sandbox).

With two arguments it compares a pair. With a directory it pairs, for every
`NN-id-TTTTms-...png`, the earliest and latest time of each root and prints one
line per pair -- which is the table in `Findings/World-effects-2026-11-09.md`
section 57.12.
"""

import os
import re
import struct
import sys
import zlib

LOW = 8  # summed |dR| + |dG| + |dB| worth reporting as "changed"
HIGH = 60  # ... and worth calling visible


def read_png(path):
    """Return (width, height, channels, pixels) for an 8-bit non-interlaced PNG."""
    data = open(path, "rb").read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit(f"{path}: not a PNG")
    pos, idat, header = 8, b"", None
    while pos < len(data):
        length = struct.unpack(">I", data[pos : pos + 4])[0]
        kind = data[pos + 4 : pos + 8]
        body = data[pos + 8 : pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            header = struct.unpack(">IIBB", body[:10])
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
    if header is None:
        raise SystemExit(f"{path}: no IHDR")
    width, height, depth, colour = header
    channels = {0: 1, 2: 3, 4: 2, 6: 4}[colour]
    if depth != 8:
        raise SystemExit(f"{path}: only 8-bit PNGs are read")
    raw = zlib.decompress(idat)
    stride = width * channels
    out = bytearray()
    previous = bytearray(stride)
    cursor = 0
    for _ in range(height):
        filter_type = raw[cursor]
        cursor += 1
        line = bytearray(raw[cursor : cursor + stride])
        cursor += stride
        if filter_type == 1:
            for x in range(channels, stride):
                line[x] = (line[x] + line[x - channels]) & 255
        elif filter_type == 2:
            for x in range(stride):
                line[x] = (line[x] + previous[x]) & 255
        elif filter_type == 3:
            for x in range(stride):
                left = line[x - channels] if x >= channels else 0
                line[x] = (line[x] + ((left + previous[x]) >> 1)) & 255
        elif filter_type == 4:
            for x in range(stride):
                left = line[x - channels] if x >= channels else 0
                up = previous[x]
                corner = previous[x - channels] if x >= channels else 0
                p = left + up - corner
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - corner)
                predictor = left if (pa <= pb and pa <= pc) else (up if pb <= pc else corner)
                line[x] = (line[x] + predictor) & 255
        elif filter_type != 0:
            raise SystemExit(f"{path}: unknown filter {filter_type}")
        out += line
        previous = line
    return width, height, channels, bytes(out)


def diff(first, second):
    width, height, channels, a = read_png(first)
    width2, height2, channels2, b = read_png(second)
    if (width, height, channels) != (width2, height2, channels2):
        raise SystemExit(f"{first} and {second} are not the same size")
    total = width * height
    changed = visible = 0
    for offset in range(0, len(a), channels):
        delta = (
            abs(a[offset] - b[offset])
            + abs(a[offset + 1] - b[offset + 1])
            + abs(a[offset + 2] - b[offset + 2])
        )
        if delta > LOW:
            changed += 1
        if delta > HIGH:
            visible += 1
    return changed * 100.0 / total, visible * 100.0 / total


PAIR = re.compile(r"^(\d+)-(\d+)-(\d+)ms(-.*)$")


def latest(directory):
    """One line per root *per camera*, earliest time against latest time.

    The camera has to be held fixed: pairing `az0/el80` with `az35/el20` counts
    the turn, not the animation, and reports a change on an effect that never
    moves. The camera is everything after the `ms` in the file name.
    """
    groups = {}
    for name in sorted(os.listdir(directory)):
        match = PAIR.match(name)
        if not match:
            continue
        groups.setdefault((match.group(1), match.group(4)), []).append((int(match.group(3)), name))
    for (index, camera) in sorted(groups):
        shots = sorted(groups[(index, camera)])
        if len(shots) < 2:
            continue
        first, last = shots[0][1], shots[-1][1]
        changed, visible = diff(os.path.join(directory, first), os.path.join(directory, last))
        print(f"{first} vs {last}: changed {changed:5.1f}% (visibly {visible:4.1f}%)")


def main(argv):
    if len(argv) == 3 and os.path.isfile(argv[1]) and os.path.isfile(argv[2]):
        changed, visible = diff(argv[1], argv[2])
        print(f"{argv[1]} vs {argv[2]}: changed {changed:.1f}% (visibly {visible:.1f}%)")
    elif len(argv) == 2 and os.path.isdir(argv[1]):
        latest(argv[1])
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
