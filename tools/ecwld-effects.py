"""Scan an ECWLD block file for effect placements (60-byte record + u32 length + GBK path).

The record layout is the one `src/scene.rs` reads: id, scale, position,
direction, up, speed, valid_time, alpha, attenuation, then a length-prefixed
path. Block files carry no header of their own, so the scan walks every offset
and keeps the ones whose length and path both look right.
"""
import struct
import sys


def strings(data, at):
    n = struct.unpack_from('<I', data, at)[0]
    if not (3 <= n <= 300) or at + 4 + n > len(data):
        return None
    raw = data[at + 4:at + 4 + n]
    if b'\0' in raw:
        return None
    text = raw.decode('gbk', 'replace')
    if not text.lower().endswith(('.gfx', '.ecm', '.ecmdl', '.bmd')) and '.gfx' not in text.lower():
        return None
    if any(ch < ' ' for ch in text):
        return None
    return text


def scan(path):
    data = open(path, 'rb').read()
    out = []
    for off in range(0, len(data) - 64):
        text = strings(data, off + 60)
        if text is None:
            continue
        export_id, scale = struct.unpack_from('<If', data, off)
        pos = struct.unpack_from('<3f', data, off + 8)
        direction = struct.unpack_from('<3f', data, off + 20)
        up = struct.unpack_from('<3f', data, off + 32)
        speed, valid_time, alpha = struct.unpack_from('<fif', data, off + 44)
        out.append((off, export_id, scale, pos, direction, up, speed, valid_time, alpha, text))
    return out


if __name__ == '__main__':
    for path in sys.argv[1:]:
        rows = scan(path)
        print(f'== {path}: {len(rows)} effect records')
        for row in rows:
            off, export_id, scale, pos, direction, up, speed, valid_time, alpha, text = row
            print(f'  {export_id:6d} scale={scale:7.3f} pos=({pos[0]:9.2f},{pos[1]:8.2f},{pos[2]:9.2f}) '
                  f'dir=({direction[0]:6.3f},{direction[1]:6.3f},{direction[2]:6.3f}) '
                  f'up=({up[0]:6.3f},{up[1]:6.3f},{up[2]:6.3f}) speed={speed:.3f} vt={valid_time} '
                  f'alpha={alpha:.2f} {text}')
