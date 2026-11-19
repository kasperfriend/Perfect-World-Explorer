#!/usr/bin/env python3
"""Hash-check matched retail motion frames and attach explicit reviewer findings."""

import argparse
import json
import math
from pathlib import Path

from lighter_capture_matrix import sha256_file, verified_image
from lighter_host_evidence import requested_target
from lighter_travel_report import PROVIDERS, read_csv, validate_pair, validate_rows

LABELS = dict(zip(("portable", "fsr", "rr"), PROVIDERS, strict=True))
ARTIFACTS = ("ghosting", "reveal_trails", "foliage_shimmer", "water_flicker", "camera_cut")


def sequence(csv, directory, plan_hash):
    meta, rows = read_csv(csv)
    receipt = validate_rows(csv, meta, rows, "motion_readback")
    if plan_hash != meta["plan_sha256"]:
        raise ValueError(f"{csv}: camera plan does not match")
    expected = {f"{i:06}.png" for i in range(len(rows))}
    if {f.name for f in directory.glob("*.png")} != expected:
        raise ValueError(f"{directory}: motion PNG sequence count or names do not match CSV")
    frames = []
    for i, row in enumerate(rows):
        stem = directory / f"{i:06}"
        verified_image(stem, {"image": meta["world"], "rgba8_sha256": row["rgba8_sha256"]})
        clock = int(row["animation_elapsed_ms"])
        depth = float(row["eye_underwater_depth"])
        cut = row["camera_cut"]
        if (clock < 16 or clock % 16 or (frames and clock <= frames[-1]["animation_elapsed_ms"])
                or cut not in ("0", "1") or not math.isfinite(depth) or depth < 0):
            raise ValueError(f"{csv}: invalid motion context at frame {i}")
        frames.append({"index": i, "path": str(stem.with_suffix(".png")),
                       "png_sha256": sha256_file(stem.with_suffix(".png")), "rgba8_sha256": row["rgba8_sha256"],
                       "pose": receipt[2][i], "animation_elapsed_ms": clock, "cut": cut == "1", "eye_underwater_depth": depth})
    return receipt, {"csv_sha256": sha256_file(csv), "metadata": meta, "frames": frames}


def matched(reports):
    baseline, base_report = reports["portable"]
    if baseline[1] not in ("Software", "Hardware") or len(set(baseline[2])) < 2:
        raise ValueError("matched motion needs one moving full-ray route, not Native or Hybrid")
    keys = ("animation_elapsed_ms", "cut", "eye_underwater_depth")
    for label, provider in LABELS.items():
        receipt, report = reports[label]
        if not requested_target(receipt[0]):
            raise ValueError(f"{label}: expected Windows x64 RTX 3080 10 GB Vulkan evidence")
        if receipt[1] != baseline[1] or receipt[0]["ray_reconstruction"] != provider:
            raise ValueError(f"{label}: wrong route or reconstruction provider; fallback is not SDK evidence")
        validate_pair(baseline, receipt)
        for a, b in zip(base_report["frames"], report["frames"], strict=True):
            for key in keys:
                if a[key] != b[key]:
                    raise ValueError(f"{label}: unmatched {key} at frame {a['index']}; rerun with matched warm-up and camera")


def review(findings, reports):
    assessed, entries = set(), []
    for row in findings:
        label, artifact = row["provider"], row["artifact"]
        if label not in reports or artifact not in ARTIFACTS:
            raise ValueError("finding must name a recorded provider and known artifact")
        frames = reports[label][1]["frames"]
        first, last, shot = (row[k] for k in ("start_frame", "end_frame", "screenshot_frame"))
        if (not all(type(n) is int for n in (first, last, shot)) or not 0 <= first <= shot <= last < len(frames)
                or row["severity"] not in ("none", "low", "medium", "high")
                or not all(isinstance(row[k], str) and row[k].strip() for k in ("location", "finding"))):
            raise ValueError("finding needs an in-range screenshot, location, severity and observation")
        if artifact == "camera_cut" and not any(f["cut"] for f in frames[first:last + 1]):
            raise ValueError("camera-cut finding needs a rendered cut in its frame range")
        entries.append(row | {"screenshot": frames[shot]})
        assessed.add((label, artifact))
    missing = sorted({(label, artifact) for label in reports for artifact in ARTIFACTS} - assessed)
    return entries, missing


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", type=Path, required=True)
    for label in (*LABELS, "fixture"):
        parser.add_argument(f"--{label}", nargs=2, metavar=("CSV", "FRAMES"), type=Path)
    parser.add_argument("--findings", type=Path, help="JSON array of reviewer observations")
    args = parser.parse_args()
    try:
        plan_hash = sha256_file(args.plan)
        paths = {label: getattr(args, label) for label in LABELS}
        if args.fixture and not any(paths.values()):
            paths = {"fixture": args.fixture}
        elif args.fixture or not all(paths.values()):
            raise ValueError("supply portable, FSR and RR sequences or one synthetic fixture")
        reports = {label: sequence(csv, frames, plan_hash) for label, (csv, frames) in paths.items()}
        if not args.fixture:
            matched(reports)
        findings = json.loads(args.findings.read_text(encoding="utf-8")) if args.findings else []
        entries, missing = review(findings, reports)
        result = {"scope": "synthetic fixture" if args.fixture else "matched motion receipt consistency; verify physical dispatch separately",
                  "plan_sha256": plan_hash, "sequences": {label: result for label, (_, result) in reports.items()},
                  "findings": entries, "unreviewed_provider_artifacts": missing}
    except (KeyError, OSError, TypeError, ValueError) as error:
        parser.error(str(error))
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
