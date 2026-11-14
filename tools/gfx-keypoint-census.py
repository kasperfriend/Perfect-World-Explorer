#!/usr/bin/env python3
"""Count keypoint modes, spans and the scales no geometry can use.

Three questions the effect player answers from the client's own grammar, and
which a per-root spot check cannot settle:

  * Does any *interpolate-mode* keypoint hold forever (`TimeSpan: -1`)? The
    client's `A3DGFXKeyPointSet::TickAnimation` runs a keypoint's controllers
    only in the `enumInterpNone` branch, whatever its span, so a mode-1 keypoint
    with `TimeSpan: -1` would be the one case where gating on the span as well
    as the mode changes the picture.
  * How many element-101 records author a non-zero `ZOffset`, the depth-only key
    `Fill_Verts_2D` reads?
  * Which keypoints hold the impossible scales §6.2 measured, and are they
    reachable at all?

`KEYPOINTCOUNT` is honoured, because the block that follows it is the only
keypoint text an element owns: a record whose keypoint list was cut short has
its remaining `InterpolateMode:` lines belonging to the *next* element, and a
naive split over the whole file counts those twice.

Usage: python3 tools/gfx-keypoint-census.py google_drive/gfx.pck
"""
import re
import sys

sys.path.insert(0, __file__.rsplit('/', 1)[0])
import pck_lib

ELEMENT = re.compile(r'GFXELEMENTID:\s*(\d+)')
FIELD = re.compile(r'^([A-Za-z_0-9]+):[ \t]*(.*)$')


def blocks(text):
    """(element id, fields) per element record, in file order.

    Splitting on `GFXELEMENTID:` alone is enough to read a file the parser reads:
    `SPEC_TERMINATORS` stops the class block at `StartTime`/`KEYPOINTCOUNT`/
    `InterpolateMode`/`AffectorCount`/`GFXELEMENTID`, so every `Scale:` line
    before the element's first `KEYPOINTCOUNT` belongs to a *controller* or to an
    element-125 generation sequence, never to a keypoint.
    """
    for part in re.split(r'(?=^GFXELEMENTID:)', text, flags=re.M):
        head = ELEMENT.match(part)
        if not head:
            continue
        fields = []
        for line in part.splitlines():
            found = FIELD.match(line)
            if found:
                fields.append((found.group(1), found.group(2)))
        yield int(head.group(1)), fields


def keypoints(fields):
    """The declared keypoint blocks of one element, and nothing after them."""
    try:
        start = next(i for i, (k, _) in enumerate(fields) if k == 'KEYPOINTCOUNT')
    except StopIteration:
        return []
    count = int(fields[start][1] or 0)
    blocks_out = []
    index = start + 1
    for _ in range(count):
        while index < len(fields) and fields[index][0] != 'InterpolateMode':
            index += 1
        if index >= len(fields):
            break
        one = {}
        while index < len(fields) and fields[index][0] not in one:
            one[fields[index][0]] = fields[index][1]
            index += 1
        blocks_out.append(one)
    return blocks_out


def main(path):
    _, entries = pck_lib.directory(path)
    gfx = [e for e in entries if e[0].lower().endswith('.gfx')]
    print(f'roots: {len(gfx)}')
    roots = elements = keypoints_seen = 0
    modes = {}
    held = []
    sprites = 0
    z101 = []
    odd_scale = []
    for name, off, usize, clen in gfx:
        roots += 1
        text = pck_lib.read_entry(path, (name, off, usize, clen)).decode('gbk', 'replace')
        for eid, fields in blocks(text):
            elements += 1
            common = dict(fields)
            # The first KEYPOINTCOUNT: everything before it is the class block.
            if eid == 101:
                sprites += 1
                value = common.get('ZOffset')
                if value is not None and float(value) != 0.:
                    z101.append((name, float(value)))
            for key in keypoints(fields):
                keypoints_seen += 1
                mode = int(key.get('InterpolateMode', 0))
                modes[mode] = modes.get(mode, 0) + 1
                span = float(key.get('TimeSpan', 0))
                if mode != 0 and span < 0:
                    held.append((name, eid, mode, span > 0))
                scale = abs(float(key.get('Scale', 1)))
                if scale > 1e6:
                    odd_scale.append((name, eid, float(key.get('Scale', 1)), span))
    print(f'elements: {elements}')
    print(f'element-101 records: {sprites}')
    print(f'101 records with a non-zero ZOffset: {len(z101)}')
    for name, value in z101[:10]:
        print(f'    {value:+8.3f}  {name}')
    print(f'keypoints: {keypoints_seen}')
    print(f'modes: {dict(sorted(modes.items()))}')
    print(f'interpolate-mode keypoints with TimeSpan: -1: {len(held)}')
    for row in held:
        print(f'    {row}')
    print(f'keypoints with |Scale| > 1e6: {len(odd_scale)}')
    for row in odd_scale:
        print(f'    {row}')


def record_dump(path, ecwld, needle):
    """The raw fields a render-size audit compares against the live log.

    The log's sizes close as `2 * Height * DedaultScale * ecwld_scale` for every
    card class (`A3DDecalEx::SetSize` takes half-extents,
    A3DDecalEx.h:167-177, and `GetActualScale` is the root's `DedaultScale`
    times the placement scale, A3DGFXEx.h:196-202), so a size that looks wrong
    is only decidable against four raw values: the root's `DedaultScale`, the
    element's `Width`/`Height`, and the ECWLD record's scale column (offset 4,
    the same one `src/scene.rs` reads). This prints all four beside the raw
    texture-mapping fields (`TexRow`/`TexCol`/`TileMode`/`TexSpeed`/mirrors)
    and `MatchGrnd`, which is what decides a floating ring (`MatchGrnd: 0` is
    authored, not a parse slip).

    Usage: python3 tools/gfx-keypoint-census.py google_drive/gfx.pck \\
               --records <name-substring> [--ecwld google_drive/world.ecwld]
    """
    import struct

    _, entries = pck_lib.directory(path)
    keep = {
        'TexFile', 'TexRow', 'TexCol', 'TexInterval', 'TileMode', 'TexSpeed',
        'UReverse', 'VReverse', 'UVExchg', 'MatchGrnd', 'GrndNormOnly',
        'Width', 'Height', 'Scale', 'RotFromView', 'SrcBlend', 'DestBlend',
    }
    for name, off, usize, clen in entries:
        if not name.lower().endswith('.gfx') or needle not in name:
            continue
        text = pck_lib.read_entry(path, (name, off, usize, clen)).decode('gbk', 'replace')
        print(f'== {name}')
        for line in text.splitlines():
            key = line.split(':', 1)[0].strip()
            if key in ('MOXTVersion', 'DedaultScale', 'PlaySpeed', 'DefaultAlpha'):
                print(f'    {line.strip()}')
        for eid, fields in blocks(text):
            print(f'  -- element {eid}')
            seen_speed = 0
            for key, value in fields:
                if key not in keep:
                    continue
                if key == 'TexSpeed':
                    seen_speed += 1
                    axis = 'UV'[seen_speed - 1] if seen_speed <= 2 else str(seen_speed)
                    key = f'TexSpeed({axis})'
                print(f'      {key}: {value}')
    if not ecwld:
        return
    data = open(ecwld, 'rb').read()
    print(f'== {ecwld} placements matching {needle!r}')
    for at in range(0, len(data) - 64):
        n = struct.unpack_from('<I', data, at + 60)[0]
        if not (3 <= n <= 300) or at + 64 + n > len(data):
            continue
        raw = data[at + 64: at + 64 + n]
        if b'\0' in raw:
            continue
        text = raw.decode('gbk', 'replace')
        if any(ch < ' ' for ch in text):
            continue
        if needle not in text:
            continue
        if not (text.lower().endswith(('.gfx', '.ecm', '.ecmdl', '.bmd')) or '.gfx' in text.lower()):
            continue
        export_id, scale = struct.unpack_from('<If', data, at)
        pos = struct.unpack_from('<3f', data, at + 8)
        speed, valid_time, alpha = struct.unpack_from('<fif', data, at + 44)
        print(
            f'    id={export_id} scale={scale:.3f} '
            f'pos=({pos[0]:.1f}, {pos[1]:.1f}, {pos[2]:.1f}) '
            f'speed={speed:.3f} vt={valid_time} alpha={alpha:.2f} {text}'
        )


if __name__ == '__main__':
    argv = sys.argv[1:]
    if '--records' in argv:
        at = argv.index('--records')
        needle = argv[at + 1] if at + 1 < len(argv) else ''
        ecwld = None
        if '--ecwld' in argv:
            ecwld = argv[argv.index('--ecwld') + 1]
        record_dump(argv[0] if at > 0 else 'google_drive/gfx.pck', ecwld, needle)
    else:
        main(argv[0] if argv else 'google_drive/gfx.pck')
