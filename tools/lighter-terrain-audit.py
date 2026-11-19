#!/usr/bin/env python3
"""Independent height-only audit of Lighter's regular or locally refined far terrain.

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
import subprocess
import sys
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

    def edges(self):
        """World-mesh triangle edges; emit shared grid boundaries once."""
        point = lambda x, y: (x, y, self.field[y * self.w + x])
        for y0, y1 in zip(self.y, self.y[1:]):
            for x0, x1 in zip(self.x, self.x[1:]):
                a, b = point(x0, y0), point(x1, y0)
                c, d = point(x0, y1), point(x1, y1)
                yield a, b
                yield a, c
                yield b, c
                if x1 == self.w - 1:
                    yield b, d
                if y1 == self.h - 1:
                    yield c, d

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



class RefinedMesh:
    """Audit the actual emitted indices, not a second refinement heuristic."""
    def __init__(self, native, step, indices, triangles):
        self.w, self.h, self.step = native.w, native.h, step
        self.nx, self.ny = len(axis(self.w, step)) - 1, len(axis(self.h, step)) - 1
        self.tiles = [[] for _ in range(self.nx * self.ny)]
        areas = [0] * len(self.tiles)
        edges = {}
        if step != 4 or len(triangles) * 3 != indices or not 0 < indices <= 600_000:
            raise ValueError("invalid production refinement budget")
        for tri in triangles:
            if len(tri) != 6 or any(not (0 <= x < self.w and 0 <= y < self.h)
                                    for x, y in zip(tri[::2], tri[1::2])):
                raise ValueError("refined terrain references an invalid source vertex")
            a, b, c = tuple((x, y, native.field[y * self.w + x]) for x, y in zip(tri[::2], tri[1::2]))
            tx, ty = min(a[0], b[0], c[0]) // step, min(a[1], b[1], c[1]) // step
            if max(a[0], b[0], c[0]) > (tx + 1) * step or max(a[1], b[1], c[1]) > (ty + 1) * step:
                raise ValueError("refined triangle crosses a tile boundary")
            area = self.cross(a, b, c[0], c[1])
            if area <= 0:
                raise ValueError("refined terrain contains a flipped or zero-area triangle")
            index = ty * self.nx + tx
            self.tiles[index].append((a, b, c, area))
            areas[index] += area
            for start, end in ((a, b), (b, c), (c, a)):
                key = tuple(sorted((start[:2], end[:2])))
                edges[key] = edges.get(key, 0) + 1
        for ((ax, ay), (bx, by)), count in edges.items():
            boundary = ax == bx and ax in (0, self.w - 1) or ay == by and ay in (0, self.h - 1)
            if count != (1 if boundary else 2):
                raise ValueError("unpaired edge in production far terrain (gap or T-junction)")
        for i, area in enumerate(areas):
            x, y = i % self.nx * step, i // self.nx * step
            if area != 2 * min(step, self.w - 1 - x) * min(step, self.h - 1 - y):
                raise ValueError("refined terrain does not cover its source rectangle")

    @staticmethod
    def cross(a, b, x, y):
        return (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0])

    def height(self, x, y):
        tx = min(self.nx - 1, int(x) // self.step)
        ty = min(self.ny - 1, int(y) // self.step)
        for a, b, c, area in self.tiles[ty * self.nx + tx]:
            wa, wb, wc = self.cross(b, c, x, y), self.cross(c, a, x, y), self.cross(a, b, x, y)
            if min(wa, wb, wc) >= 0:
                return (wa * a[2] + wb * b[2] + wc * c[2]) / area
        raise ValueError("height probe outside refined mesh")

    def edges(self):
        for tile in self.tiles:
            for a, b, c, _ in tile:
                yield a, b
                yield b, c
                yield c, a

    def profile_knots(self, placement, *, row):
        i, j = (0, 1) if row else (1, 0)
        if row:
            tiles = (self.tiles[y * self.nx + min(self.nx - 1, placement // self.step)] for y in range(self.ny))
        else:
            start = min(self.ny - 1, placement // self.step) * self.nx
            tiles = iter(self.tiles[start : start + self.nx])
        points = set()
        for tile in tiles:
            for a, b, c, _ in tile:
                for p, q in ((a, b), (b, c), (c, a)):
                    if p[i] == placement:
                        points.add((p[j], p[2]))
                    if q[i] == placement:
                        points.add((q[j], q[2]))
                    if min(p[i], q[i]) < placement < max(p[i], q[i]):
                        t = (placement - p[i]) / (q[i] - p[i])
                        points.add((p[j] + t * (q[j] - p[j]), p[2] + t * (q[2] - p[2])))
        return points


def production_mesh(native, planner):
    source = native.field
    if sys.byteorder != "little":
        source = source[:]
        source.byteswap()
    done = subprocess.run([str(planner.resolve()), str(native.w), str(native.h)], input=source.tobytes(),
                          capture_output=True, timeout=60)
    if done.returncode:
        raise ValueError(f"production planner failed: {done.stderr.decode(errors='replace').strip()}")
    header, *triangles = done.stdout.decode("ascii").splitlines()
    words = header.split()
    if len(words) == 3 and words[0] == "regular" and not triangles:
        step = int(words[1])
        if step not in (1, 2, 4, 16):
            raise ValueError("unexpected regular far stride")
        mesh = Mesh(native.field, native.w, native.h, step)
        return step, mesh, {"source": "production", "kind": "regular", "stride": step,
                            "indices": 6 * (len(mesh.x) - 1) * (len(mesh.y) - 1)}
    if len(words) != 5 or words[0] != "adaptive":
        raise ValueError("unexpected production far-layout header")
    step, indices, refined = map(int, words[1:4])
    return "adaptive", RefinedMesh(native, step, indices, [tuple(int(n) for n in tri.split()) for tri in triangles]), {
        "source": "production", "kind": "adaptive", "base_stride": step, "indices": indices,
        "refined_tiles": refined, "planner_continuous_max_abs": float(words[4]),
        "emitted_layout_sha256": hashlib.sha256(done.stdout).hexdigest(),
    }

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
    # Uniform-only estimate, retained to compare the old viewer or diagnose
    # the large-map fallback. --viewer-planner runs the production policy.
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


def diagonal_envelopes(mesh, across_sign, angles):
    """Exactly intersect triangle edges with integer diagonal slice lines.

    Plane x +/- y = k; along-slice projection is (x -/+ y) / sqrt(2).
    Every maximum of the piecewise-linear silhouette is an edge knot;
    do not miss one that falls between the height-grid samples.
    """
    offset = 0 if across_sign == 1 else mesh.h - 1
    projected = [[-math.inf] * (mesh.w + mesh.h - 1) for _ in angles]

    def add(k, along, height):
        for profile, (cos, sin) in zip(projected, angles, strict=True):
            profile[k] = max(profile[k], cos * height + sin * along)

    for (x0, y0, z0), (x1, y1, z1) in mesh.edges():
        k0, k1 = (x0 + across_sign * y0 + offset, x1 + across_sign * y1 + offset)
        p0, p1 = (x0 - across_sign * y0, x1 - across_sign * y1)
        add(k0, p0, z0)
        add(k1, p1, z1)
        for k in range(min(k0, k1) + 1, max(k0, k1)):
            q = (k - k0) / (k1 - k0)
            add(k, p0 + (p1 - p0) * q, z0 + (z1 - z0) * q)
    if any(not math.isfinite(h) for profile in projected for h in profile):
        raise ValueError("missing projected terrain on a diagonal profile")
    return projected


def compare(native, far, spacing, limit, diagonal=False):
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
    if diagonal:
        keys = [(degree, sign) for degree in (1, 2, 3) for sign in (1, -1)]
        angles = [(math.cos(math.radians(deg)), sign * math.sin(math.radians(deg)) * spacing / math.sqrt(2))
                  for deg, sign in keys]
        for azimuth, across in (("NE_SW", 1), ("NW_SE", -1)):
            n = diagonal_envelopes(native, across, angles)
            f = diagonal_envelopes(far, across, angles)
            for (deg, sign), ref, test in zip(keys, n, f, strict=True):
                key = f"{azimuth}_{deg}deg_{'forward' if sign == 1 else 'reverse'}"
                report["grazing"][key] = stats((abs(a - b) for a, b in zip(ref, test, strict=True)), limit)
    return report


def audit(path, strides, cutoff, limit, planner=None, diagonal=False):
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
    if planner is not None:
        name, mesh, report["layout"] = production_mesh(native, planner)
        layouts = [(name, mesh)]
    else:
        report["layout"] = {"source": "requested strides" if strides else "uniform-only estimate"}
        layouts = [(stride, Mesh(field, w, h, stride)) for stride in (strides or [viewer_stride(native)])]
    for stride, mesh in layouts:
        result = compare(native, mesh, spacing, limit, diagonal)
        report["strides"][stride] = result
        if any(stat["over_limit"] for stat in (result["height_grid"], *result["grazing"].values())):
            report["status"] = "geometry_error"
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trn2", nargs="+", type=Path, help="each TRN2 must be beside its T2BK files")
    parser.add_argument("--stride", action="append", type=int, choices=[1, 2, 4, 8, 16], help="override stride; repeat for multiple comparisons (default: uniform-only estimate)")
    parser.add_argument("--viewer-planner", type=Path, help="compiled lighter-terrain-plan binary (audit actual emitted indices)")
    parser.add_argument("--diagonal-grazing", action="store_true", help="add exact edge-knot profiles along both diagonal azimuths")
    parser.add_argument("--limit", default=0.5, type=float, help="maximum height/silhouette error in world units")
    parser.add_argument("--cutoff-window", nargs=4, type=int, metavar=("X0", "Y0", "X1", "Y1"), help="model an intentional near-only block window; still validate source heights")
    parser.add_argument("--json", type=Path, help="write the full per-view statistics as JSON")
    args = parser.parse_args()
    if not 0 < args.limit < math.inf:
        parser.error("--limit must be positive and finite")
    if sum(map(bool, (args.cutoff_window, args.stride, args.viewer_planner))) > 1:
        parser.error("select a cutoff window, explicit stride(s), or the production planner")
    results = []
    for path in args.trn2:
        try:
            report = audit(path, args.stride, args.cutoff_window, args.limit, args.viewer_planner, args.diagonal_grazing)
        except (ValueError, OSError, struct.error, zlib.error, subprocess.TimeoutExpired) as error:
            report = {"map": path.stem, "status": "load_failed", "error": str(error)}
        results.append(report)
        print(f"{report['map']}: {report['status']}")
        if "error" in report:
            print(f"  {report['error']}")
        if "cutoff_blocks" in report:
            print(f"  {report['cutoff_blocks']} of {report['decoded_blocks']} loaded blocks intentionally omitted")
        if "indices" in report.get("layout", {}):
            layout = report["layout"]
            print(f"  production {layout['kind']}: {layout['indices']} indices")
        for stride, stats_by_kind in report.get("strides", {}).items():
            heights = stats_by_kind["height_grid"]
            name, worst = max(stats_by_kind["grazing"].items(), key=lambda item: item[1]["max_abs"])
            label = "adaptive" if stride == "adaptive" else f"stride {stride}"
            print(f"  {label}: height mean {heights['mean_abs']:.4f}, max {heights['max_abs']:.4f} "
                  f"({heights['over_limit']}/{heights['samples']} beyond {args.limit}); "
                  f"worst silhouette {worst['max_abs']:.4f} ({name})")
    if args.json:
        args.json.write_text(json.dumps({"limit": args.limit, "results": results}, indent=2) + "\n", encoding="utf-8")
    return max({"load_failed": 2, "geometry_error": 1}.get(report["status"], 0) for report in results)


if __name__ == "__main__":
    raise SystemExit(main())
