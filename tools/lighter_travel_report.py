#!/usr/bin/env python3
"""Summarize normal-present Lighter travel CSVs without treating them as GPU/display latency."""
import argparse
import csv
import json
import math
from pathlib import Path
import re
import statistics

MATCH = ("map", "hour", "plan_sha256", "adapter", "display", "world", "present_mode", "view_sha256", "view", "warmup")
RAY_MATCH = ("trace", "screen_cache_mask", "compact_terrain", "sampled_bc")
POSE = ("x", "y", "z", "yaw", "pitch")


def percentile(values, p):
    return sorted(values)[math.ceil(p * len(values)) - 1]


def durations(values, cadence=False):
    if not values:
        return None
    result = {"n": len(values), "p50_ms": statistics.median(values), "p95_ms": percentile(values, 0.95),
              "p99_ms": percentile(values, 0.99), "max_ms": max(values)}
    if cadence:
        tail = sorted(values, reverse=True)[:max(1, math.ceil(len(values) / 100))]
        result["slowest_one_percent_fps"] = 1000 / statistics.mean(tail)
    return result


def capture(path):
    with Path(path).open(encoding="utf-8", newline="") as f:
        meta = {}
        for line in f:
            if not line.startswith("# "):
                rows = list(csv.DictReader(f, fieldnames=line.strip().split(",")))
                break
            key, value = line[2:].rstrip("\r\n").split("=", 1)
            if key in meta:
                raise ValueError(f"{path}: duplicate field {key}")
            meta[key] = value
        else:
            raise ValueError(f"{path}: no frame rows")
    if (meta.get("schema") != "1" or not 30 <= len(rows) <= 30_000 or int(meta["count"]) != len(rows)
            or [int(r["frame"]) for r in rows] != list(range(len(rows)))):
        raise ValueError(f"{path}: incomplete or unexpected travel schema")
    for key in MATCH + RAY_MATCH + ("ray", "hybrid"):
        if not meta.get(key):
            raise ValueError(f"{path}: missing {key}")
    route = re.match(r"Settings \{ mode: (Off|Software|Hardware),", meta["ray"])
    if route is None or meta["hybrid"] not in ("true", "false"):
        raise ValueError(f"{path}: invalid route")
    mode = route[1]
    if meta["hybrid"] == "true":
        if mode != "Hardware":
            raise ValueError(f"{path}: raster primary requires Hardware")
        mode = "Hybrid"
    result, poses = [], []
    for row in rows:
        pose = tuple(float(row[k]) for k in POSE)
        d = float(row["interval_ms"])
        redraw = float(row["redraw_ms"])
        acquire = float(row["acquire_ms"])
        present = float(row["present_call_ms"])
        heap = int(row["device_local_heap_estimate_bytes"]) if row["device_local_heap_estimate_bytes"] else None
        if (not all(map(math.isfinite, (*pose, d, redraw, acquire, present))) or d <= 0 or min(redraw, acquire, present) < 0
                or any(row[k] not in ("0", "1") for k in ("streaming", "committed"))
                or (heap is not None and heap < 0)):
            raise ValueError(f"{path}: invalid sample {row['frame']}")
        poses.append(pose)
        result.append((d, redraw, row["streaming"] == "1", row["committed"] == "1", heap, acquire, present))
    return meta, mode, poses, result


def validate_pair(a, b):
    for key in MATCH:
        if a[0][key] != b[0][key]:
            raise ValueError(f"unmatched travel {key}: {a[0][key]} vs {b[0][key]}")
    if a[2] != b[2]:
        raise ValueError("recorded camera paths disagree")
    if "Off" not in (a[1], b[1]):
        normalize = lambda meta: re.sub(r"(mode: \w+, |hybrid_primary: (true|false), )", "", meta["ray"])
        for key in RAY_MATCH:
            if a[0][key] != b[0][key]:
                raise ValueError(f"unmatched ray {key}: {a[0][key]} vs {b[0][key]}")
        if normalize(a[0]) != normalize(b[0]):
            raise ValueError("effective ray settings disagree")


def summarize(data, require_commit=False, require_heap=False, physical=False, device=None):
    meta, mode, _, rows = data
    is_physical_vulkan = "backend: Vulkan" in meta["adapter"] and bool(re.search(r"device_type: (DiscreteGpu|IntegratedGpu)", meta["adapter"]))
    if physical and not is_physical_vulkan:
        raise ValueError("physical results require a physical Vulkan adapter")
    if device and device.casefold() not in meta["adapter"].casefold():
        raise ValueError(f"adapter does not match {device!r}")
    stable = [d for d, _, streaming, commit, *_ in rows if not streaming and not commit]
    loading = [d for d, _, streaming, commit, *_ in rows if streaming or commit]
    heap = [r[4] for r in rows if r[4] is not None]
    commits = sum(r[3] for r in rows)
    if require_commit and commits == 0:
        raise ValueError("no resident commit was observed during this path")
    if require_heap and not heap:
        raise ValueError("this run did not report device-local heap usage")
    return {"mode": mode, "physical_vulkan": is_physical_vulkan, "metadata": meta,
            "normal_present_calls": durations([r[0] for r in rows], cadence=True),
            "redraw_to_present_return": durations([r[1] for r in rows]),
            "surface_acquire_cpu": durations([r[5] for r in rows]),
            "present_call_cpu": durations([r[6] for r in rows]),
            "stable_intervals": durations(stable), "streaming_or_commit_intervals": durations(loading),
            "stream_commits": commits, "heap_samples": len(heap),
            "peak_sampled_device_local_heap_estimate_bytes": max(heap, default=None)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("csv", type=Path, nargs="+")
    parser.add_argument("--physical", action="store_true")
    parser.add_argument("--require-commit", action="store_true")
    parser.add_argument("--require-heap", action="store_true")
    parser.add_argument("--device", help="require a substring of the recorded adapter description")
    args = parser.parse_args()
    try:
        captures = [capture(path) for path in args.csv]
        for b in captures[1:]:
            validate_pair(captures[0], b)
        # Also compare all ray runs, even if the first capture was Native.
        rays = [r for r in captures if r[1] != "Off"]
        for b in rays[1:]:
            validate_pair(rays[0], b)
        result = [summarize(r, args.require_commit, args.require_heap, args.physical, args.device) for r in captures]
    except (OSError, ValueError, TypeError, KeyError, ZeroDivisionError) as exc:
        parser.error(str(exc))
    print(json.dumps({"timing_scope": "normal wgpu present calls, not display latency",
                      "memory_scope": "sampled Vulkan process device-local heap estimate, not exact VRAM residency",
                      "captures": result}, indent=2))


if __name__ == "__main__":
    main()
