#!/usr/bin/env python3
"""Independent height-only audit of Lighter's regular far terrain grid.

Reads actual TRN2/T2BK heights without the client, masks, or a graphics device.
Compares the production triangle split to the unstitched stride-one surface at
every native vertex and cell center. Grazing orthographic silhouettes are the
top envelope of the piecewise-linear mesh in four cardinal directions. This is
geometry evidence, not a viewer capture or a material/parity test.
"""
import argparse
import array
import hashlib
import json
import math
from pathlib import Path
import struct
import zlib


def word(data, offset):
    return struct.unpack_from("<I", data, offset)[0]


def tile_heights(path, grid, expected, sources):
    data = path.read_bytes()
    sources[path.name] = hashlib.sha256(data).hexdigest()
    magic, version, count = struct.unpack_from("<3I", data)
    if magic != 0x54424B42 or not 2 <= version <= 6 or count < expected:
        raise ValueError(f"{path.name}: invalid T2BK header or missing blocks")
    table_end = 16 + count * 4
    if table_end > len(data):
        raise ValueError(f"{path.name}: truncated block offset table")
    flags = data[12] if version >= 5 else 0x15 if data[12] else 0
    if flags & ~0x35:
        raise ValueError(f"{path.name}: unsupported T2BK flags 0x{flags:02x}")
    header = 36 if version >= 6 else 32 if version >= 4 else 28 if version >= 3 else 24
    nv = (grid + 1) ** 2
    starts = struct.unpack_from(f"<{count}I", data, 16) + (len(data),)
    for block, (offset, end) in enumerate(zip(starts, starts[1:])):
        if not table_end <= offset <= offset + header <= end <= len(data):
            raise ValueError(f"{path.name}: block {block}: invalid offset span")
        hs, ns = struct.unpack_from("<2I", data, offset + 16)
        start = offset + header
        tail = start + hs + ns
        if tail > end or bool(end - tail) != bool(flags & 1):
            raise ValueError(f"{path.name}: block {block}: inconsistent section lengths")
        if block >= expected:
            continue
        raw = data[start : start + hs]
        if flags & 4:
            inflater = zlib.decompressobj()
            raw = inflater.decompress(raw, nv * 4 + 1)
            if not inflater.eof:
                raise ValueError(f"{path.name}: block {block}: invalid zlib heights")
        if len(raw) != nv * 4:
            raise ValueError(f"{path.name}: block {block}: wrong height count")
        samples = struct.unpack_from(f"<{nv}f", raw)
        if not all(map(math.isfinite, samples)):
            raise ValueError(f"{path.name}: block {block}: non-finite height")
        yield samples


def load(path):
    trn = path.read_bytes()
    sources = {path.name: hashlib.sha256(trn).hexdigest()}
    if word(trn, 0) != 0x54524E32 or word(trn, 4) > 1 or word(trn, 76) != 1:
        raise ValueError("unsupported TRN2 header")
    grid, rows, cols = (word(trn, offset) for offset in (20, 28, 32))
    count, subgrid, offset = (word(trn, offset) for offset in (52, 64, 80))
    spacing = struct.unpack_from("<f", trn, 68)[0]
    if not (1 <= grid <= 256 and 0 < rows <= 4096 and 0 < cols <= 4096 and subgrid
            and subgrid % grid == 0 and 0 < spacing < math.inf):
        raise ValueError("invalid terrain dimensions")
    per = subgrid // grid
    if rows % per or cols % per or count != (rows // per) * (cols // per) or count > 10000:
        raise ValueError("inconsistent subterrain dimensions")
    width, height = cols * grid, rows * grid
    if min(width, height) < 2 or width * height > 100_000_000:
        raise ValueError("unsupported heightfield extent")
    names = []
    offset += 4
    for _ in range(count):
        length = word(trn, offset)
        offset += 4
        if not 0 < length <= 4096 or offset + length > len(trn):
            raise ValueError("invalid subterrain name length")
        name = trn[offset : offset + length].decode("utf-8")
        if any(char in name for char in ("/", "\\", "..", "\0", ":")):
            raise ValueError("unsafe subterrain name")
        names.append(name)
        offset += length
    field = array.array("f", [0.0]) * (width * height)
    across = cols // per
    for tile, name in enumerate(names):
        for block, samples in enumerate(tile_heights(path.parent / (name + ".t2bk"), grid, per * per, sources)):
            row = ((tile // across) * per + block // per) * grid
            col = ((tile % across) * per + block % per) * grid
            # Match the viewer: drop each block's last row and column. The
            # adjacent block owns the shared sample; the world grid stops at w-1/h-1.
            for dy in range(grid):
                start = (row + dy) * width + col
                at = dy * (grid + 1)
                field[start : start + grid] = array.array("f", samples[at : at + grid])
    return field, width, height, grid, spacing, sources


def axis(size, stride):
    return [*range(0, size - 1, stride), size - 1]


class Mesh:
    def __init__(self, field, width, height, stride):
        self.field, self.w, self.h, self.stride = field, width, height, stride
        self.x, self.y = axis(width, stride), axis(height, stride)

    def height(self, x, y):
        x0 = int(min(x, self.w - 2)) // self.stride * self.stride
        y0 = int(min(y, self.h - 2)) // self.stride * self.stride
        x1, y1 = min(x0 + self.stride, self.w - 1), min(y0 + self.stride, self.h - 1)
        u, v = (x - x0) / (x1 - x0), (y - y0) / (y1 - y0)
        b, c = self.field[y0 * self.w + x1], self.field[y1 * self.w + x0]
        if u + v <= 1:
            return (1 - u - v) * self.field[y0 * self.w + x0] + u * b + v * c
        return (1 - v) * b + (u + v - 1) * self.field[y1 * self.w + x1] + (1 - u) * c

    def profile_knots(self, placement, *, row):
        """Heights of the mesh along a line, including its diagonal crossings."""
        along = self.y if row else self.x
        across = self.w if row else self.h
        base = int(min(placement, across - 2)) // self.stride * self.stride
        edge = min(base + self.stride, across - 1)
        u = (placement - base) / (edge - base)
        knots = along if u == 0.0 or u == 1.0 else (
            along + [a + (b - a) * (1 - u) for a, b in zip(along, along[1:])]
        )
        return [(k, self.height(placement, k) if row else self.height(k, placement)) for k in knots]


def stats(errors, limit):
    n, total, peak, beyond = 0, 0.0, 0.0, 0
    for delta in errors:
        n += 1
        total += delta
        peak = max(peak, delta)
        beyond += delta > limit
    return {"samples": n, "mean_abs": total / n, "max_abs": peak, "over_limit": beyond}


def height_error(native, far, limit):
    w, h = native.w, native.h
    def errors():
        for y in range(h):
            for x in range(w):
                yield abs(native.field[y * w + x] - far.height(x, y))
        for y in range(h - 1):
            for x in range(w - 1):
                yield abs(native.height(x + 0.5, y + 0.5) - far.height(x + 0.5, y + 0.5))
    return stats(errors(), limit)


def viewer_stride(native):
    # Mirror terrain_fidelity.rs; --stride overrides this to compare the old
    # viewer or the large-map fallback even on a small authored heightfield.
    selected = (math.inf, 16)
    for stride in (4, 2, 1):
        far = Mesh(native.field, native.w, native.h, stride)
        if 6 * (len(far.x) - 1) * (len(far.y) - 1) > 600_000:
            break
        error = 0.0 if stride == 1 else height_error(native, far, 0.5)["max_abs"]
        if error < selected[0]:
            selected = error, stride
        if error <= 0.5:
            break
    return selected[1]


def compare(native, far, spacing, limit):
    w, h = native.w, native.h
    report = {"height_grid": height_error(native, far, limit), "grazing": {}}
    for axis_name, across, row in [("rows", w, True), ("columns", h, False)]:
        profiles = {
            (degrees, sign): []
            for degrees in (1, 2, 3) for sign in (1, -1)
        }
        # Envelope extrema of projected triangle segments are exactly at the
        # knots. Opposite views reverse the along-axis projection sign.
        angles = [(degrees, math.cos(math.radians(degrees)), math.sin(math.radians(degrees)) * spacing)
                  for degrees in (1, 2, 3)]
        for placement in range(across):
            n = native.profile_knots(placement, row=row)
            f = far.profile_knots(placement, row=row)
            for degrees, cos, sin in angles:
                for sign in (1, -1):
                    ref = max(y * cos + k * sin * sign for k, y in n)
                    test = max(y * cos + k * sin * sign for k, y in f)
                    profiles[degrees, sign].append(abs(ref - test))
        report["grazing"].update({f"{axis_name}_{degrees}deg_{'forward' if sign == 1 else 'reverse'}": stats(errors, limit)
                                  for (degrees, sign), errors in profiles.items()})
    return report


def audit(path, strides, cutoff, limit):
    field, w, h, grid, spacing, sources = load(path)
    report = {"map": path.stem, "heightfield": [w, h], "spacing": spacing,
              "decoded_blocks": (w // grid) * (h // grid), "source_sha256": sources}
    if cutoff:
        x0, y0, x1, y1 = cutoff
        if not (0 <= x0 < x1 <= w // grid and 0 <= y0 < y1 <= h // grid):
            raise ValueError("cutoff window must be in loaded block coordinates")
        report["cutoff_blocks"] = report["decoded_blocks"] - (x1 - x0) * (y1 - y0)
        report["status"] = "intentional_cutoff"
        return report
    report["status"] = "within_tolerance"
    report["strides"] = {}
    native = Mesh(field, w, h, 1)
    strides = strides or [viewer_stride(native)]
    for stride in strides:
        result = compare(native, Mesh(field, w, h, stride), spacing, limit)
        report["strides"][stride] = result
        if any(stat["over_limit"] for stat in (result["height_grid"], *result["grazing"].values())):
            report["status"] = "geometry_error"
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trn2", nargs="+", type=Path, help="each TRN2 must be beside its T2BK files")
    parser.add_argument("--stride", action="append", type=int, choices=[1, 2, 4, 8, 16], help="override stride; repeat for multiple comparisons (default: current viewer policy)")
    parser.add_argument("--limit", default=0.5, type=float, help="maximum height/silhouette error in world units")
    parser.add_argument("--cutoff-window", nargs=4, type=int, metavar=("X0", "Y0", "X1", "Y1"), help="model an intentional near-only block window; still validate source heights")
    parser.add_argument("--json", type=Path, help="write the full per-view statistics as JSON")
    args = parser.parse_args()
    if not 0 < args.limit < math.inf:
        parser.error("--limit must be positive and finite")
    if args.cutoff_window and args.stride:
        parser.error("select either a cutoff window or a far stride")
    results = []
    for path in args.trn2:
        try:
            report = audit(path, args.stride, args.cutoff_window, args.limit)
        except (ValueError, OSError, struct.error, zlib.error) as error:
            report = {"map": path.stem, "status": "load_failed", "error": str(error)}
        results.append(report)
        print(f"{report['map']}: {report['status']}")
        if "error" in report:
            print(f"  {report['error']}")
        if "cutoff_blocks" in report:
            print(f"  {report['cutoff_blocks']} of {report['decoded_blocks']} loaded blocks intentionally omitted")
        for stride, stats_by_kind in report.get("strides", {}).items():
            heights = stats_by_kind["height_grid"]
            name, worst = max(stats_by_kind["grazing"].items(), key=lambda item: item[1]["max_abs"])
            print(f"  stride {stride}: height mean {heights['mean_abs']:.4f}, max {heights['max_abs']:.4f} "
                  f"({heights['over_limit']}/{heights['samples']} beyond {args.limit}); "
                  f"worst silhouette {worst['max_abs']:.4f} ({name})")
    if args.json:
        args.json.write_text(json.dumps({"limit": args.limit, "results": results}, indent=2) + "\n", encoding="utf-8")
    return max({"load_failed": 2, "geometry_error": 1}.get(report["status"], 0) for report in results)


if __name__ == "__main__":
    raise SystemExit(main())
