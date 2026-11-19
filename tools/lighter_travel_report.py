#!/usr/bin/env python3
"""Summarize normal-present Lighter travel CSVs without treating them as GPU/display latency."""
import argparse
import csv
import hashlib
import json
import math
from pathlib import Path
import re
import statistics

from lighter_host_evidence import TARGET_ADAPTER, requested_target

MATCH = ("map", "hour", "plan_sha256", "adapter", "display", "world", "present_mode", "view_sha256", "view", "warmup")
RAY_MATCH = ("trace", "screen_cache_mask", "compact_terrain", "sampled_bc")
POSE = ("x", "y", "z", "yaw", "pitch")
STAGES = ("queue_drain_wall_ms", "gpu_as_ms", "gpu_raster_primary_ms", "gpu_trace_ms", "gpu_reproject_ms",
          "gpu_filter_ms", "gpu_ray_composite_ms", "gpu_sdk_ms", "gpu_native_world_span_ms")
PROVIDERS = ("portable", "Vulkan temporal FSR", "Vulkan DLSS Ray Reconstruction")


def percentile(values, p):
    return sorted(values)[math.ceil(p * len(values)) - 1]


def durations(values, cadence=False):
    if not values:
        return None
    result = {"n": len(values), "p50_ms": statistics.median(values), "p95_ms": percentile(values, 0.95),
              "p99_ms": percentile(values, 0.99), "max_ms": max(values)}
    if cadence:
        tail = sorted(values, reverse=True)[:max(1, math.ceil(len(values) / 100))]
        result["slowest_one_percent_ms"] = statistics.mean(tail)
        result["slowest_one_percent_fps"] = 1000 / result["slowest_one_percent_ms"]
    return result


def read_csv(path):
    with Path(path).open(encoding="utf-8", newline="") as f:
        meta = {}
        for line in f:
            if not line.startswith("# "):
                columns = next(csv.reader([line]))
                rows = list(csv.DictReader(f, fieldnames=columns))
                if len(columns) != len(set(columns)) or any(None in r or None in r.values() for r in rows):
                    raise ValueError(f"{path}: malformed travel columns")
                return meta, rows
            key, value = line[2:].rstrip("\r\n").split("=", 1)
            if key in meta:
                raise ValueError(f"{path}: duplicate field {key}")
            meta[key] = value
    raise ValueError(f"{path}: no frame rows")


def capture(path, synchronized=False):
    meta, rows = read_csv(path)
    return validate_rows(path, meta, rows, "synchronized" if synchronized else "normal_present")


def validate_rows(path, meta, rows, policy):
    synchronized = policy == "synchronized"
    motion = policy == "motion_readback"
    if meta.get("capture_policy", "normal_present") != policy:
        raise ValueError(f"{path}: expected a {policy} performance sample; exclude motion/readback")
    if rows and not synchronized and any(stage in rows[0] for stage in STAGES):
        raise ValueError(f"{path}: GPU stage fields in a non-synchronized sample")
    provider = meta.get("ray_reconstruction", "portable")
    if provider not in PROVIDERS or ((synchronized or motion) and "ray_reconstruction" not in meta):
        raise ValueError(f"{path}: invalid or missing provider receipt")
    if (meta.get("schema") != "1" or not (500 if synchronized else 30) <= len(rows) <= (600 if motion else 30_000) or int(meta["count"]) != len(rows)
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
    if mode in ("Off", "Hybrid") and provider != "portable":
        raise ValueError(f"{path}: provider incompatible with {mode}")
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
        stages = None
        if synchronized:
            stages = tuple(float(row[k]) if row.get(k) else None for k in STAGES)
            if stages[0] is None or any(x is not None and (not math.isfinite(x) or x < 0) for x in stages):
                raise ValueError(f"{path}: invalid synchronized stages in frame {row['frame']}")
            if provider == "portable" and stages[7] is not None:
                raise ValueError(f"{path}: portable route reported an SDK timestamp")
            if mode != "Off" and stages[8] is not None:
                raise ValueError(f"{path}: ray route reported a Native world timestamp")
        result.append((d, redraw, row["streaming"] == "1", row["committed"] == "1", heap, acquire, present, stages))
    return meta, mode, poses, result


def validate_pair(a, b, reuse_ab=False):
    for key in MATCH:
        if a[0][key] != b[0][key]:
            raise ValueError(f"unmatched travel {key}: {a[0][key]} vs {b[0][key]}")
    for key in ("platform", "architecture", "vulkan_largest_device_local_heap_bytes"):
        if a[0].get(key) != b[0].get(key):
            raise ValueError(f"unmatched travel {key}")
    if a[2] != b[2]:
        raise ValueError("recorded camera paths disagree")
    if "Off" not in (a[1], b[1]):
        normalize = lambda meta: re.sub(r"(mode: \w+, |hybrid_primary: (true|false), )", "", meta["ray"])
        for key in RAY_MATCH:
            if reuse_ab and key == "screen_cache_mask":
                continue
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
    peak = max(heap, default=None)
    if "peak_sampled_device_local_heap_estimate_bytes" in meta:
        reported = int(meta["peak_sampled_device_local_heap_estimate_bytes"])
        if peak is not None and reported < peak:
            raise ValueError("driver heap summary is below a per-frame sample")
        peak = reported
    stages = {key: durations([r[7][i] for r in rows if r[7] is not None and r[7][i] is not None])
              for i, key in enumerate(STAGES)}
    return {"mode": mode, "physical_vulkan": is_physical_vulkan, "metadata": meta,
            "provider": meta.get("ray_reconstruction", "portable"),
            "synchronized_stages": stages if meta.get("capture_policy") == "synchronized" else None,
            ("synchronized_present_calls" if meta.get("capture_policy") == "synchronized" else "normal_present_calls"):
                durations([r[0] for r in rows], cadence=True),
            "redraw_to_present_return": durations([r[1] for r in rows]),
            "surface_acquire_cpu": durations([r[5] for r in rows]),
            "present_call_cpu": durations([r[6] for r in rows]),
            "stable_intervals": durations(stable), "streaming_or_commit_intervals": durations(loading),
            "stream_commits": commits, "heap_samples": len(heap),
            "peak_sampled_device_local_heap_estimate_bytes": peak,
            "heap_at_or_above_7_gb": None if peak is None else peak >= 7_000_000_000}


def route_key(record):
    return record[1], record[0].get("ray_reconstruction", "portable")


def validate_suite(records, *, synchronized=True):
    required = {("Off", "portable")} | {(mode, provider) for mode in ("Software", "Hardware") for provider in PROVIDERS}
    pairs = list(map(route_key, records))
    if len(pairs) != len(required) or set(pairs) != required:
        raise ValueError(f"missing or duplicate {'synchronized' if synchronized else 'normal-present'} routes: {sorted(required - set(pairs))}")
    for meta, mode, _, rows in records:
        if not requested_target(meta):
            raise ValueError("SDK matrix needs Windows x64 RTX 3080 10 GB Vulkan evidence")
        provider = meta["ray_reconstruction"]
        heap = [r[4] for r in rows if r[4] is not None]
        if not heap or min(heap) <= 0 or (synchronized and len(heap) != len(rows)):
            raise ValueError(f"{mode}/{provider}: whole-viewer heap samples are missing")
        peak = int(meta.get("peak_sampled_device_local_heap_estimate_bytes", "0"))
        if peak < max(heap) or meta.get("heap_at_or_above_7_gb") != str(peak >= 7_000_000_000).lower():
            raise ValueError(f"{mode}/{provider}: inconsistent sampled heap peak or warning receipt")
        if not synchronized:
            continue
        required_stages = ({8} if mode == "Off" else {3, 6})
        if mode == "Hardware":
            required_stages.add(1)
        if mode != "Off":
            required_stages.update((4, 5) if provider == "portable" else (7,))
        for row in rows:
            sample = row[7]
            if sample is None:
                raise ValueError(f"{mode}/{provider}: no synchronized stage sample")
            for stage in required_stages:
                v = sample[stage]
                if v is None or (stage in (3, 7, 8) and v <= 0):
                    raise ValueError(f"{mode}/{provider}: missing current-frame {STAGES[stage]}")


def validate_matched(captures, *, reuse_ab=False):
    groups = (captures, [r for r in captures if r[1] != "Off"])
    for group in groups:
        for b in group[1:]:
            validate_pair(group[0], b, reuse_ab=reuse_ab)


def matched_normal_companions(synchronized, normal):
    validate_suite(normal, synchronized=False)
    controls = {route_key(r): r for r in normal}
    for r in synchronized:
        validate_pair(r, controls[route_key(r)])
    return [controls[route_key(r)] for r in synchronized]


def receipt_summary(path, record, **kwargs):
    return summarize(record, **kwargs) | {"csv": str(path), "csv_sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("csv", type=Path, nargs="+")
    parser.add_argument("--physical", action="store_true")
    parser.add_argument("--synchronized", action="store_true", help="read separate >=500-frame queue-completion runs")
    parser.add_argument("--require-sdk-matrix", action="store_true", help="require all seven RTX 3080 synchronized routes")
    parser.add_argument("--normal-companions", nargs="+", type=Path, metavar="CSV",
                        help="validate separate, exactly matched normal-present CSVs for the seven synchronized routes")
    parser.add_argument("--reuse-ab", action="store_true", help="allow only a matched default/both cache A/B pair")
    parser.add_argument("--require-commit", action="store_true")
    parser.add_argument("--require-heap", action="store_true")
    parser.add_argument("--device", help="require a substring of the recorded adapter description")
    args = parser.parse_args()
    try:
        sync = args.synchronized or args.require_sdk_matrix
        captures = [capture(path, synchronized=sync) for path in args.csv]
        if args.require_sdk_matrix:
            validate_suite(captures)
        if args.reuse_ab:
            if (len(captures) != 2 or captures[0][1] not in ("Software", "Hardware") or captures[0][1] != captures[1][1]
                    or any(r[0].get("ray_reconstruction", "portable") != "portable" for r in captures)
                    or {int(r[0]["screen_cache_mask"]) & 3 for r in captures} != {0, 3}):
                raise ValueError("reuse A/B requires two matched portable ray runs, one default and one both")
        validate_matched(captures, reuse_ab=args.reuse_ab)
        device = TARGET_ADAPTER if args.require_sdk_matrix else args.device
        options = dict(require_commit=args.require_commit, require_heap=args.require_heap,
                       physical=args.physical or args.require_sdk_matrix, device=device)
        result = [receipt_summary(p, r, **options) for p, r in zip(args.csv, captures, strict=True)]
        companions = None
        if args.normal_companions:
            if not args.require_sdk_matrix:
                raise ValueError("--normal-companions requires --require-sdk-matrix")
            normal = [capture(path) for path in args.normal_companions]
            ordered = matched_normal_companions(captures, normal)
            paths = {route_key(r): p for p, r in zip(args.normal_companions, normal, strict=True)}
            companions = [receipt_summary(paths[route_key(r)], r, **options) for r in ordered]
    except (OSError, ValueError, TypeError, KeyError, ZeroDivisionError) as exc:
        parser.error(str(exc))
    timing = ("queue drained before each wgpu present; wall time includes completion polling, not display latency"
              if sync else "normal wgpu present calls, not display latency")
    print(json.dumps({"timing_scope": timing,
                      "memory_scope": "sampled Vulkan process device-local heap estimate, not exact VRAM residency",
                      "native_gpu_scope": "world queue timestamp span; may include queue idle, excludes UI/present/scanout",
                      "captures": result, "normal_present_companions": companions}, indent=2))


if __name__ == "__main__":
    main()
