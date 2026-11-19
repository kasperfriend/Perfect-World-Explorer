#!/usr/bin/env python3
"""Capture matched retail cameras; never substitute a CPU driver for Hardware.

Requires Pillow for PNG decoding and region statistics. Client bytes are not
copied into the output, only per-frame images, sidecars, logs and the report.
"""

import argparse
import csv
import hashlib
from itertools import combinations
import json
import math
import os
from pathlib import Path
import re
import statistics
import subprocess
from datetime import datetime, timezone

from PIL import Image, ImageChops


PROFILES = {
    "cpu": ("off", "software"),
    "physical": ("off", "software", "hardware"),
}
REUSE = {"off": 0, "di": 1, "gi": 2, "both": 3}
CLASSES = {"city", "forest", "open_distance", "water", "cave_instance", "effects"}
CONDITIONS = {"day": (12, "Clear"), "night": (0, "Clear"), "rain": (12, "Rain")}
MATCH_KEYS = (
    "map", "eye", "yaw", "pitch", "image", "far", "fov_y", "hour", "weather",
    "warmup_frames", "warmup_step_ms", "animation_elapsed_ms", "cutoff", "scenery_quality",
    "reflection", "fog", "effects", "cloud_detail", "bloom", "lightmaps", "area_grass_lightmaps",
    "rain_at_eye", "weather_ticks", "adapter",
)
REGIONS = {
    "whole": (0, 0, 1, 1),
    "upper": (0, 0, 1, 1 / 3),
    "middle": (0, 1 / 3, 1, 2 / 3),
    "lower": (0, 2 / 3, 1, 1),
    "center": (0.35, 0.35, 0.65, 0.65),
}


def metadata(path):
    fields = dict(line.split("=", 1) for line in path.read_text(encoding="utf-8").splitlines() if "=" in line)
    if fields.get("coverage_schema") != "3":
        raise ValueError(f"{path}: expected matched capture schema 3")
    return fields


def validate_match(baseline, candidate, *, compare_primary=False):
    for key in MATCH_KEYS:
        if baseline.get(key) is None or baseline[key] != candidate.get(key):
            raise ValueError(f"unmatched {key}: {baseline.get(key)!r} vs {candidate.get(key)!r}")
    # Native has no ray settings. All ray variants use exactly the same
    # transport, except for the explicitly requested primary-visibility A/B.
    if "Off" not in (baseline["mode"], candidate["mode"]):
        a, b = (re.sub(r"mode: \w+, ", "", m["settings_effective"], count=1) for m in (baseline, candidate))
        if compare_primary:
            a, b = (re.sub(r"hybrid_primary: (?:true|false), ", "", s, count=1) for s in (a, b))
        if a != b:
            raise ValueError("ray modes have different effective settings")
        for key in ("bc_terrain_tiles", "bc_terrain_saved_bytes", "screen_reuse_flags"):
            if baseline.get(key, "0") != candidate.get(key, "0"):
                raise ValueError(f"unmatched {key}: {baseline.get(key)!r} vs {candidate.get(key)!r}")


def regions_for(size, custom=None):
    width, height = size
    for name, bounds in (REGIONS | (custom or {})).items():
        if len(bounds) != 4:
            raise ValueError(f"invalid region {name}: {bounds}")
        x0, y0, x1, y1 = bounds
        if not (0 <= x0 < x1 <= 1 and 0 <= y0 < y1 <= 1):
            raise ValueError(f"invalid region {name}: {bounds}")
        box = (round(x0 * width), round(y0 * height), round(x1 * width), round(y1 * height))
        if box[0] >= box[2] or box[1] >= box[3]:
            raise ValueError(f"empty pixel region {name}: {box}")
        yield name, box


def compare(a, b, custom=None):
    if a.size != b.size:
        raise ValueError(f"different image extents: {a.size} vs {b.size}")
    diff = ImageChops.difference(a.convert("RGB"), b.convert("RGB"))
    summary = {}
    for name, box in regions_for(a.size, custom):
        pixels = (box[2] - box[0]) * (box[3] - box[1])
        hist = diff.crop(box).histogram()
        channel_mae = [sum(i * n for i, n in enumerate(hist[j:j + 256])) / pixels for j in (0, 256, 512)]
        summary[name] = {
            "box_xyxy": box,
            "pixels": pixels,
            "mean_abs_rgb8": round(statistics.mean(channel_mae), 5),
            "channel_mean_abs_rgb8": [round(v, 5) for v in channel_mae],
        }
    return summary, diff


def capture_metadata(stem, mode, case, profile):
    meta = metadata(stem.with_suffix(".coverage.txt"))
    if (json.loads(meta["map"]) != case["map"] or meta["mode"].lower() != mode
            or (float(meta["hour"]), meta["weather"]) != CONDITIONS[case["condition"]]
            or int(meta["warmup_frames"]) != case["warmup"]
            or int(meta["warmup_step_ms"]) != 16
            or int(meta["animation_elapsed_ms"]) != 16 * case["warmup"]
            or (meta["rain_at_eye"] == "true") != (case["condition"] == "rain")
            or (meta["lightmaps"] == "true") != case["lightmaps"]):
        raise ValueError(f"{stem.name}: capture doesn't match the case")
    adapter = meta["adapter"]
    device = re.search(r"device_type: (\w+)", adapter)
    if not device or "backend: Vulkan" not in adapter:
        raise ValueError(f"{stem.name}: missing Vulkan adapter information")
    if profile == "cpu" and device[1] != "Cpu":
        raise ValueError(f"{stem.name}: CPU evidence needs a CPU adapter")
    if profile == "physical" and device[1] not in ("DiscreteGpu", "IntegratedGpu"):
        raise ValueError(f"{stem.name}: physical evidence cannot use {device[1]}")
    if case.get("camera") is not None:
        pose = json.loads(meta["eye"]) + [float(meta["yaw"]), float(meta["pitch"])]
        if not all(math.isclose(a, b, abs_tol=0.0001, rel_tol=1e-7) for a, b in zip(pose, case["camera"], strict=True)):
            raise ValueError(f"{stem.name}: captured the wrong camera")
    with Image.open(stem.with_suffix(".png")) as source:
        image = source.convert("RGBA")
    if f"{image.width}x{image.height}" != meta["image"]:
        raise ValueError(f"{stem.name}: extent disagrees with metadata")
    if hashlib.sha256(image.tobytes()).hexdigest() != meta["rgba8_sha256"]:
        raise ValueError(f"{stem.name}: decoded RGBA8 hash disagrees with metadata")
    return meta, image


def inventory_for(stem, case):
    with stem.with_suffix(".coverage.csv").open(newline="", encoding="utf-8") as stream:
        rows = {r["class"]: {key: int(value) for key, value in r.items() if key != "class"} for r in csv.DictReader(stream)}
    if rows["near_terrain"]["loaded"] <= 0:
        raise ValueError(f"{stem.name}: no near terrain")
    for cls, expected in case.get("authored_totals", {}).items():
        total = sum(rows[cls][key] for key in ("loaded", "skipped", "failed"))
        if total != expected:
            raise ValueError(f"{stem.name}: {cls} accounted for {total}, not {expected}")
    for cls, expected in case.get("min_loaded", {}).items():
        if rows[cls]["loaded"] < expected:
            raise ValueError(f"{stem.name}: {cls} loaded {rows[cls]['loaded']}, expected at least {expected}")
    return rows


def gpu_stage_samples(path):
    """Optional GPU counters, separate from synchronized wall time."""
    samples = []
    with path.open(encoding="utf-8", errors="replace") as log:
        for line in log:
            fields = line.strip().split()
            if fields[:3] == ["Lighter", "gpu", "stage"] and len(fields) > 4:
                sample = {"stage_source": fields[3].rstrip(":")}
                for key, value in (token.split("=", 1) for token in fields[4:] if "=" in token):
                    if value != "raw-vulkan":
                        sample[key] = int(value) if key.endswith("_bytes") else float(value)
                samples.append(sample)
    return samples


STAGES = ("build", "primary", "traversal", "reproject", "filter", "upscale")


def completed_stages(meta, count):
    # Optional measurements paired with the synchronized timed draws, not
    # the periodic stage samples printed independently of capture wall time.
    values = json.loads(meta.get("gpu_timed_ms", "[]"))
    if not values:
        return None
    if (json.loads(meta["gpu_timed_stages"]) != ",".join(STAGES) or len(values) > count
            or any(len(row) != len(STAGES) or any(not math.isfinite(t) or t < 0 for t in row) for row in values)):
        raise ValueError("invalid completed GPU stage samples")
    return {"stages": STAGES, "samples": values,
            "p50": dict(zip(STAGES, map(statistics.median, zip(*values, strict=True)), strict=True))}


def run_case(case, args):
    name = case["id"]
    if not re.fullmatch(r"[a-z0-9-]+", name):
        raise ValueError(f"unsafe case ID {name!r}")
    profile = args.profile
    mode_results, images = {}, {}
    variants = PROFILES[profile] + (("hybrid",) if args.hybrid_ab else ())
    for variant in variants:
        raster = variant == "hybrid"
        mode = "hardware" if raster else variant
        stem = args.output / f"{name}-{variant}"
        hour, weather = CONDITIONS[case["condition"]]
        env = {k: v for k, v in os.environ.items() if not k.startswith("PW_PREVIEW_")}
        for key in ("PW_TEST_NO_LIGHTMAPS", "PW_RAY_COMPACT_TERRAIN", "PW_RAY_BC_TEXTURES", "PW_RAY_SCREEN_REUSE"):
            env.pop(key, None)
        env.update(WGPU_BACKEND="vulkan", PW_PREVIEW_COVERAGE="1", PW_PREVIEW_LIGHTER=mode,
                   PW_PREVIEW_RAY_RESOLUTION=str(args.ray_resolution), PW_PREVIEW_ANISOTROPY="8",
                   PW_PREVIEW_HOUR=str(hour), PW_PREVIEW_WEATHER=weather, PW_PREVIEW_WARMUP=str(case["warmup"]),
                   PW_PREVIEW_TIMED_FRAMES=str(args.timed_frames))
        if raster:
            env["PW_PREVIEW_HYBRID_PRIMARY"] = "1"
        if mode != "off":
            if args.progressive or args.screen_reuse != "off":
                env["PW_PREVIEW_PROGRESSIVE"] = "1"
            if args.compact_terrain:
                env["PW_RAY_COMPACT_TERRAIN"] = "1"
            if args.fsr1_easu:
                env["PW_PREVIEW_FSR1_EASU"] = "1"
            if args.screen_reuse != "off":
                env["PW_RAY_SCREEN_REUSE"] = args.screen_reuse
        if not case["lightmaps"]:
            env["PW_TEST_NO_LIGHTMAPS"] = "1"
        cmd = [str(args.viewer.resolve()), str(args.client.resolve()), "--world", case["map"]]
        if case.get("camera") is not None:
            cmd += ["--gpu-preview-at", *map(str, case["camera"])]
        else:
            cmd += ["--gpu-preview"]
        cmd.append(str(stem.with_suffix(".png").resolve()))
        path = stem.with_suffix(".log")
        print(f"{name}: {variant}", flush=True)
        try:
            with path.open("wb") as log:
                proc = subprocess.run(cmd, env=env, stdout=log, stderr=subprocess.STDOUT, timeout=args.timeout)
            if proc.returncode != 0:
                raise RuntimeError(f"viewer exited {proc.returncode}")
        except (RuntimeError, subprocess.TimeoutExpired) as error:
            tail = "\n".join(path.read_text(errors="replace").splitlines()[-12:])
            raise RuntimeError(f"{stem.name}: {error}\n{tail}") from error
        meta, images[variant] = capture_metadata(stem, mode, case, profile)
        if meta.get("raster_primary", "false") != str(raster).lower():
            raise ValueError(f"{stem.name}: requested and recorded primary path differ")
        requested_reuse = REUSE[args.screen_reuse] if mode != "off" else 0
        if (int(meta.get("screen_reuse_flags", "0")) & 3) != requested_reuse:
            raise ValueError(f"{stem.name}: requested screen reuse not admitted")
        rows = inventory_for(stem, case) if mode != "off" else None
        timings = json.loads(meta.get("synchronized_capture_ms", "[]"))
        if len(timings) != args.timed_frames or any(not math.isfinite(t) or t < 0 for t in timings):
            raise ValueError(f"{stem.name}: invalid synchronized timing samples")
        durations = None if not timings else {
            "samples": timings,
            "p50": statistics.median(timings),
            "p95": sorted(timings)[math.ceil(0.95 * len(timings)) - 1],
        }
        mode_results[variant] = {
            "files": stem.name,
            "meta": meta,
            "inventory": rows,
            "synchronized_capture_ms": durations,
            "gpu_completed_stage_ms": completed_stages(meta, len(timings)),
            "gpu_stage_samples": gpu_stage_samples(path),
        }
    comparisons = {}
    for first, second in combinations(variants, 2):
        validate_match(mode_results[first]["meta"], mode_results[second]["meta"], compare_primary=second == "hybrid")
        regions, diff = compare(images[first], images[second], case.get("regions"))
        pair = f"{first}-vs-{second}"
        diff.save(args.output / f"{name}-{pair}-abs.png")
        comparisons[pair] = regions
    return {"case": case, "captures": mode_results, "comparisons": comparisons}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("manifest", "viewer", "client", "output"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    parser.add_argument("--profile", choices=PROFILES, default="physical")
    parser.add_argument("--ray-resolution", type=int, default=50, choices=range(25, 101), metavar="25..100")
    parser.add_argument("--timed-frames", type=int, choices=range(121), default=0, metavar="0..120")
    parser.add_argument("--timeout", type=int, default=360, help="seconds per capture")
    parser.add_argument("--hybrid-ab", action="store_true", help="also capture Hardware with rasterized 1-spp primary visibility")
    parser.add_argument("--compact-terrain", action="store_true", help="use the experimental buffer-backed BC terrain path")
    parser.add_argument("--fsr1-easu", action="store_true", help="use the spatial EASU color resolve on the ray modes")
    parser.add_argument("--progressive", action="store_true", help="accumulate ray history without requiring a reuse cache")
    parser.add_argument("--screen-reuse", choices=REUSE, default="off", help="experimental 1-spp reuse; enables progressive history")
    parser.add_argument("--require-complete", action="store_true", help="fail unless all required physical matrix cells exist")
    args = parser.parse_args()
    if args.hybrid_ab and args.profile != "physical":
        parser.error("--hybrid-ab requires --profile physical")
    cases = json.loads(args.manifest.read_text(encoding="utf-8"))["cases"]
    if not cases or len({case["id"] for case in cases}) != len(cases):
        parser.error("expected nonempty, unique case identifiers")
    covered = {(cls, case["condition"]) for case in cases for cls in case["classes"]}
    missing = sorted({(cls, condition) for cls in CLASSES for condition in CONDITIONS} - covered)
    complete = not missing and args.profile == "physical"
    if args.require_complete:
        if not complete:
            parser.error(f"physical matrix is incomplete: profile={args.profile}, missing cells={missing}")
        if args.timed_frames < 3:
            parser.error("complete physical runs need at least three --timed-frames per mode")
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()):
        parser.error(f"use a fresh output directory: {args.output}")
    report = {"started_utc": datetime.now(timezone.utc).isoformat(), "source_sha": os.environ.get("GITHUB_SHA"),
              "profile": args.profile, "options": {"compact_terrain": args.compact_terrain,
                                                     "fsr1_easu": args.fsr1_easu, "screen_reuse": args.screen_reuse,
                                                     "progressive": args.progressive or args.screen_reuse != "off"},
              "declared_physical_matrix_complete": complete, "missing_scene_conditions": missing,
              "metric": "mean absolute difference of encoded RGB8 pixel channels (lower is closer, not a pass threshold)",
              "cases": []}
    report["errors"] = {}
    prefix = "::notice::" if os.environ.get("GITHUB_ACTIONS") == "true" else ""
    for case in cases:
        try:
            result = run_case(case, args)
        except (ValueError, RuntimeError, OSError, KeyError) as error:
            report["errors"][case["id"]] = str(error)
            print(f"{case['id']}: {error}", flush=True)
            continue
        report["cases"].append(result)
        software = result["captures"]["software"]
        metrics = result["comparisons"]["off-vs-software"]
        bmd = software["inventory"]["buildings"]
        roots = software["inventory"]["effect_roots"]
        summary = ", ".join(f"{key}:{r['mean_abs_rgb8']:.3f}" for key, r in metrics.items())
        timing = "; ".join(f"{mode} p50/p95 {t['p50']:.1f}/{t['p95']:.1f} ms"
                           for mode, data in result["captures"].items()
                           if (t := data["synchronized_capture_ms"]) is not None)
        print(f"{prefix}{case['id']} RGB8 MAE {summary}; ray BMD {bmd['loaded']}/{bmd['skipped']}/{bmd['failed']}; "
              f"GFX roots {roots['loaded']}/{roots['skipped']}/{roots['failed']}; "
              f"eye {software['meta']['eye']} yaw {software['meta']['yaw']} pitch {software['meta']['pitch']}; "
              f"{timing or 'no synchronized timing requested'}", flush=True)
        if len(report["cases"]) == 1:
            print(f"{prefix}{software['meta']['adapter']}", flush=True)
    report["declared_physical_matrix_complete"] = complete and not report["errors"] and args.timed_frames >= 3
    (args.output / "matrix.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    if report["errors"]:
        message = f"Failed capture cases: {', '.join(report['errors'])}; retained partial evidence in matrix.json"
        print(f"{'::error::' if prefix else ''}{message}", flush=True)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
