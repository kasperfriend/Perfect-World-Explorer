#!/usr/bin/env python3
"""Cross-check every drawn card against the size its element authored.

A card's extent is `2 * Width/Height * GetActualScale * kp.m_fScale` for the
classes that draw a card (`A3DDecalEx`, `A3DGFXModel`'s keypoint, the particle
families, the ring's per-section strip), and a class that multiplies by anything
else stands out here as a size no element of that root could have produced.

The three channels are joined through the root:

  effect-elements  `#N id=.. tex=".." size=WxH`   what the file authored
  effect-frames    `id=.. kp=.. scale=S`          the keypoint that ticked
  effect-cards     `card W x H ... tex ..`        what was submitted

`scale=` in the frames channel is the keypoint scale, and the root's
`GetActualScale` (DefScale x record scale) is the placement's basis length, which
the cards do not print — so the audit takes it from the render header line, which
does.

Usage: tools/effect-size-audit.py [directory]     (default Findings/effect-preview)
"""

import re
import sys
from pathlib import Path

TOLERANCE = 0.02


def header_scale(render, elements):
    """The root's `GetActualScale`: the placement's `fScale` times `DedaultScale`.

    The render header prints the ECWLD record's own `fScale`, and
    `A3DGFXEx::CalcActualScale` (A3DGFXEx.h:202-206) multiplies the file's
    `DedaultScale` into it, so the two lines have to be joined.
    """
    defaults = {}
    for line in elements.read_text(encoding='utf-8', errors='replace').splitlines():
        match = re.search(r'gfx/(\S+\.gfx) version=\d+ default_scale=([\d.]+)', line)
        if match:
            defaults[match.group(1)] = float(match.group(2))
    scales = {}
    for line in render.read_text(encoding='utf-8', errors='replace').splitlines():
        match = re.search(r'`[^`]+` — (\S+\.gfx) .* scale ([\d.]+)', line)
        if match:
            root, record = match.group(1), float(match.group(2))
            scales.setdefault(root, record * defaults.get(root, 1.0))
    return scales


def elements(path):
    """root -> [(id, texture, width, height)] from the element table."""
    table = {}
    for line in path.read_text(encoding='utf-8', errors='replace').splitlines():
        match = re.search(
            r'gfx/(\S+\.gfx) #\d+ id=(\d+) name="[^"]*" tex="([^"]*)" .*'
            r'size=([\d.]+)x([\d.]+)', line)
        if match:
            root, ident, tex, width, height = match.groups()
            table.setdefault(root, []).append(
                (int(ident), tex.split('\\\\')[-1], float(width), float(height)))
    return table


def keypoints(path):
    """root -> [(id, kp scale)] from the frame channel."""
    table = {}
    for line in path.read_text(encoding='utf-8', errors='replace').splitlines():
        match = re.search(r'gfx/(\S+\.gfx) id=(\d+) kp=\d+ .*?scale=([\d.]+)', line)
        if match:
            table.setdefault(match.group(1), []).append(
                (int(match.group(2)), float(match.group(3))))
    return table


def cards(path):
    """root -> [(width, height, texture)], taken from the card dump."""
    found = []
    root = None
    for line in path.read_text(encoding='utf-8', errors='replace').splitlines():
        header = re.search(r'`[^`]+` — (\S+\.gfx) ', line)
        if header:
            root = header.group(1)
            continue
        match = re.search(
            r'- card ([\d.]+) x ([\d.]+) at .* tex (gfx/\S+)', line)
        if match and root:
            found.append((root, float(match.group(1)), float(match.group(2)),
                          match.group(3)))
    return found


def main(argv):
    directory = Path(argv[1] if len(argv) > 1 else 'Findings/effect-preview')
    table = elements(directory / 'effect-elements.log.txt')
    roots = header_scale(directory / 'render.log.txt',
                         directory / 'effect-elements.log.txt')
    kps = keypoints(directory / 'effect-frames.log.txt')
    drawn = cards(directory / 'render.log.txt')

    print(f'{len(drawn)} cards over {len(roots)} roots')
    unexplained = 0
    for root, width, height, texture in drawn:
        root_scale = roots.get(root)
        if root_scale is None:
            print(f'  ? {root}: no placement scale in the render log')
            continue
        # Every (element, keypoint) pair of this root that names this texture.
        expected = []
        for ident, tex, ew, eh in table.get(root, []):
            # Only the decal classes are checked. Ids 120 to 126 are the
            # particle families (`ParticleWidth` times a per-particle scale from
            # `ScaleMin..ScaleMax`, reported by `effect-particles` with the world
            # size it computed); 140 is the ring, whose strip is
            # `circumference / sects` wide and `Height` tall, not `2 x Height`;
            # and 150/190/210/240 build from authored vertices rather than a
            # Width/Height pair.
            if ident not in (100, 101, 102):
                continue
            if ew <= 0. or eh <= 0.:
                continue
            if not tex or tex != texture.split('/')[-1]:
                continue
            for kp_id, kp in kps.get(root, []):
                if kp_id != ident:
                    continue
                expected.append((2 * ew * root_scale * kp, 2 * eh * root_scale * kp))
        if not expected:
            continue
        # A `RotFromView` card's corners are the client's `verts[0..3]`, where
        # `verts[1] - verts[0]` is the height and `verts[0] - verts[2]` the
        # width; the dump reads the first pair as its first number, so the two
        # swap for an axial card. Compare as a set.
        def near(a, b):
            return abs(a - b) <= TOLERANCE * max(b, 1e-6)

        ok = any((near(width, w) and near(height, h))
                 or (near(width, h) and near(height, w))
                 for w, h in expected)
        if not ok:
            unexplained += 1
            want = ', '.join(f'{w:.2f}x{h:.2f}' for w, h in sorted(set(expected))[:4])
            print(f'  ! {root}: card {width:.2f}x{height:.2f} {texture.split("/")[-1]}'
                  f' — authored would be {want}')

    print(f'{unexplained} card(s) match no authored size')
    return 0 if unexplained == 0 else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv))
