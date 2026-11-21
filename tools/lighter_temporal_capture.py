#!/usr/bin/env python3
"""Run the ordered Step 6 preview suite on the Windows RTX target.

A manifest contains one scouted camera per scene. Keep this separate from the
live motion/timing runs; a PNG comparison does not certify SDK quality.
"""

import argparse
import json
import math
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys

from lighter_capture_matrix import (case_has_input_pins, regions_for, sha256_file,
                                    verify_inputs)
from lighter_temporal_compare import PROVIDERS, SCENES, compare_temporal, settings_field

ROUTES = ("software", "hardware")
WARMUP = 48
SIZE = (960, 720)


def read_cases(manifest):
    cases = json.loads(manifest.read_text(encoding="utf-8"))["cases"]
    names = [case["scene"] for case in cases]
    if len(names) != len(SCENES) or set(names) != set(SCENES):
        raise ValueError("Supply exactly one case for each of: " + ", ".join(SCENES))
    ordered = []
    for scene in SCENES:
        case = next(c for c in cases if c["scene"] == scene)
        pose = case["camera"]
        if (not re.fullmatch(r"[a-z0-9_-]+", case["map"]) or len(pose) != 5
                or not all(math.isfinite(x) for x in pose) or not -1.5 <= pose[-1] <= 1.5
                or not 0 <= case["hour"] < 24 or case.get("weather", "Clear") not in ("Clear", "Rain", "Snow")
                or not 0 <= case["reference_mae_limit"] <= 255):
            raise ValueError(f"{scene}: invalid map, camera, weather, clock or refinement limit")
        if not case.get("regions") or not isinstance(case["regions"], dict):
            raise ValueError(f"{scene}: name at least one reviewed subject ROI")
        list(regions_for(SIZE, case["regions"]))
        reviewed = manifest.parent / case["camera_evidence"]
        if not reviewed.is_file() or not reviewed.stat().st_size:
            raise ValueError(f"{scene}: missing camera evidence: {reviewed}")
        flight = None
        if case.get("path"):
            flight = manifest.parent / case["path"]
            records = [line.split("#", 1)[0].split() for line in flight.read_text(encoding="utf-8").splitlines()]
            records = [row for row in records if row]
            if (len(records) < 3 or [records[0][0], int(records[0][2])] != [case["map"], WARMUP]
                    or float(records[0][1]) != case["hour"] or records[1][0] != "0"
                    or int(records[-1][0]) != WARMUP or list(map(float, records[-1][1:])) != pose
                    or len({tuple(map(float, row[1:])) for row in records[1:]}) < 2):
                raise ValueError(f"{scene}: expected a moving 0..48 plan ending at the scouted pose")
            # The viewer checks monotonic frame indexes, finite poses and
            # staying in the final terrain block before drawing anything.
        elif scene in ("path", "reveal"):
            raise ValueError(f"{scene}: provide a moving warm-up path")
        ordered.append((case, reviewed, flight))
    return ordered


def preview_env(case, route, label, path, resolution, reference_samples):
    env = {k: v for k, v in os.environ.items() if not k.startswith(("PW_PREVIEW_", "PW_TEST_"))}
    for key in ("PW_DISABLE_WORLD_MODELS", "PW_SMALL_TREE_ATLAS", "PW_UI_TEST", "PW_SKIP_TREES_TEST",
                "PW_RAY_COMPACT_TERRAIN", "PW_RAY_BC_TEXTURES", "PW_RAY_SCREEN_REUSE"):
        env.pop(key, None)
    sdk = label if label in ("fsr", "rr") else "off"
    env.update(WGPU_BACKEND="vulkan", PW_PREVIEW_COVERAGE="1", PW_PREVIEW_LIGHTER=route,
               PW_PREVIEW_WARMUP=str(WARMUP), PW_PREVIEW_HOUR=str(case["hour"]),
               PW_PREVIEW_WEATHER=case.get("weather", "Clear"), PW_PREVIEW_RAY_RESOLUTION=str(resolution),
               PW_PREVIEW_REQUIRE_PROVIDER=sdk if sdk != "off" else "portable", PW_RAY_RECONSTRUCTION=sdk)
    if path is not None:
        env["PW_PREVIEW_PATH_PLAN"] = str(path.resolve())
    if label.startswith("reference"):
        env["PW_PREVIEW_REFERENCE"] = "1"
        env["PW_PREVIEW_REFERENCE_SPP"] = str(reference_samples if label == "reference_low" else 2 * reference_samples)
    return env


def run_case(args, out, case, review, flight, route):
    out.mkdir(parents=True)
    staged = Path(shutil.copyfile(flight, out / "camera.txt")) if flight is not None else None
    report = {"camera_evidence": {"name": review.name, "sha256": sha256_file(review)},
              "requested": case, "route": route, "errors": {}, "capture_files": {}}
    stems = {label: out / label for label in PROVIDERS}
    for label, stem in stems.items():
        env = preview_env(case, route, label, staged, args.ray_resolution, args.reference_samples)
        cmd = [str(args.viewer.resolve()), str(args.client.resolve()), "--world", case["map"],
               "--gpu-preview-at", *map(str, case["camera"]), str(stem.with_suffix(".png").resolve())]
        print(f"{case['scene']} {route} {label}", flush=True)
        with stem.with_suffix(".log").open("wb") as log:
            try:
                subprocess.run(cmd, env=env, stdout=log, stderr=subprocess.STDOUT, timeout=args.timeout, check=True)
            except (OSError, subprocess.SubprocessError) as error:
                report["errors"][label] = str(error)
        report["capture_files"][label] = {p.name: sha256_file(p) for p in out.glob(label + ".*") if p.is_file()}
    if not report["errors"]:
        try:
            comparison = compare_temporal(stems, case["scene"], case["regions"], case["reference_mae_limit"])
            meta = comparison["captures"]["off"]["meta"]
            pose = json.loads(meta["eye"]) + [float(meta["yaw"]), float(meta["pitch"])]
            if (json.loads(meta["map"]) != case["map"] or comparison["mode"].lower() != route
                    or meta["image"] != "x".join(map(str, SIZE)) or int(settings_field(meta, "resolution")) != args.ray_resolution
                    or float(meta["hour"]) != case["hour"] or meta["weather"] != case.get("weather", "Clear")
                    or not all(math.isclose(a, b, abs_tol=1e-4) for a, b in zip(pose, case["camera"], strict=True))
                    or meta.get("camera_path_sha256") != (sha256_file(staged) if staged else None)):
                raise ValueError("rendered camera, map, route, weather or path did not match the manifest")
            report["comparison"] = comparison
            if comparison["reference_refinement_failed_regions"]:
                report["errors"]["reference"] = "Refined Reference exceeds the predeclared ROI error budget"
        except (OSError, ValueError, KeyError, TypeError) as error:
            report["errors"]["comparison"] = str(error)
    (out / "receipt.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return not report["errors"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for field in ("manifest", "viewer", "client", "input-manifest", "output"):
        parser.add_argument(f"--{field}", type=Path, required=True)
    parser.add_argument("--source-sha", required=True, help="full commit used to build the viewer")
    parser.add_argument("--ray-resolution", type=int, choices=(50, 100), default=50)
    parser.add_argument("--reference-samples", type=int, choices=(32, 64), default=32, help="lower budget; high is double")
    parser.add_argument("--timeout", type=int, default=1800, help="seconds per preview command")
    args = parser.parse_args()
    try:
        if sys.platform != "win32" or platform.machine().lower() not in ("amd64", "x86_64"):
            raise ValueError("physical capture requires Windows x64; no CPU substitution")
        if not re.fullmatch(r"[a-f0-9]{40}", args.source_sha) or args.timeout <= 0:
            raise ValueError("expected a full source SHA and positive preview timeout")
        cases = read_cases(args.manifest)
        pins = verify_inputs(args.client, args.input_manifest)
        for case, _, _ in cases:
            if not case_has_input_pins(case, pins):
                raise ValueError(f"{case['scene']}: pin the shared packages and map records")
        report = {"source_sha": args.source_sha, "viewer_sha256": sha256_file(args.viewer),
                  "plan_sha256": sha256_file(args.manifest), "input_manifest_sha256": sha256_file(args.input_manifest),
                  "verified_inputs": pins, "cases": {},
                  "scope": "matched physical receipts; inspect subjects and retain SDK dispatch validation separately"}
        args.output.mkdir(parents=True, exist_ok=True)
        if any(args.output.iterdir()):
            raise ValueError("use a fresh output directory")
    except (OSError, KeyError, ValueError, TypeError, IndexError) as error:
        parser.error(str(error))
    for case, review, flight in cases:
        for route in ROUTES:
            name = f"{case['scene']}-{route}"
            report["cases"][name] = run_case(args, args.output / name, case, review, flight, route)
            (args.output / "index.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return not all(report["cases"].values())


if __name__ == "__main__":
    raise SystemExit(main())
