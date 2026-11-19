#!/usr/bin/env python3
"""Compare one matched RTX Lighter preview using sidecars and two Reference budgets.

All input arguments name PNG stems (omit .png). This checks capture receipts,
not host provenance or the SDK output buffer; retain the physical dispatch
smoke, machine/driver receipts and the tested source commit separately.
"""

import argparse
import json
import math
from pathlib import Path
import re

from lighter_capture_matrix import MATCH_KEYS, compare, metadata, validate_match, verified_image

PROVIDERS = {
    "off": "portable",
    "fsr": "Vulkan temporal FSR",
    "rr": "Vulkan DLSS Ray Reconstruction",
    "reference_low": "portable",
    "reference": "portable",
}
SCENES = ("still", "path", "actor", "thin", "wet", "layered", "reveal")


def settings_field(meta, name):
    match = re.search(rf"\b{name}: (\w+)(?:,| \}})", meta["settings_effective"])
    if not match:
        raise ValueError(f"Missing effective setting: {name}")
    return match[1]


def validate_temporal(metas, scene):
    off = metas["off"]
    if off["mode"] not in ("Software", "Hardware"):
        raise ValueError("off is the portable ray control, not Native")
    adapter = off["adapter"]
    if (off.get("platform") != "windows" or 'name: "NVIDIA GeForce RTX 3080"' not in adapter or "backend: Vulkan" not in adapter
            or "device_type: DiscreteGpu" not in adapter or "vendor: 4318" not in adapter):
        raise ValueError("expected the physical Windows NVIDIA GeForce RTX 3080 Vulkan adapter")
    for label, expected in PROVIDERS.items():
        meta = metas[label]
        if meta.get("ray_reconstruction") != expected:
            raise ValueError(f"{label}: requested {expected}, got {meta.get('ray_reconstruction')}")
        if meta["mode"] != off["mode"]:
            raise ValueError(f"{label}: ray route changed")
        is_reference = label.startswith("reference")
        if settings_field(meta, "reference") != str(is_reference).lower():
            raise ValueError(f"{label}: mismatched Reference mode")
        if any(meta.get(k) != v for k, v in (
            ("warmup_frames", "48"), ("warmup_step_ms", "16"), ("animation_elapsed_ms", "768"),
            ("raster_primary", "false"), ("screen_reuse_flags", "0"),
        )):
            raise ValueError(f"{label}: expected full-primary, no reuse, fixed 48×16 ms warm-up")
        if settings_field(meta, "dynamic_resolution") != "false":
            raise ValueError(f"{label}: adaptive resolution")
        if is_reference:
            if (settings_field(meta, "bounces") != "8" or settings_field(meta, "resolution") != "100"
                    or settings_field(meta, "progressive") != "false" or settings_field(meta, "denoiser") != "false"):
                raise ValueError(f"{label}: expected full-resolution unfiltered Reference")
        else:
            if settings_field(meta, "samples") != "1":
                raise ValueError(f"{label}: expected 1 spp")
            validate_match(off, meta)
        for key in (*MATCH_KEYS, "platform", "camera_path_sha256"):
            if meta.get(key) != off.get(key):
                raise ValueError(f"{label}: unmatched {key}")
    if scene in ("path", "reveal") and not re.fullmatch("[0-9a-f]{64}", off.get("camera_path_sha256", "")):
        raise ValueError(f"{scene}: requires a matched moving warm-up camera plan")
    low, high = metas["reference_low"], metas["reference"]
    if int(settings_field(low, "samples")) < 32 or int(settings_field(high, "samples")) < 2 * int(settings_field(low, "samples")):
        raise ValueError("Reference refinement needs at least 32 spp and a doubled budget")
    normalize = lambda meta: re.sub(r"\bsamples: \d+", "samples: <refined>", meta["settings_effective"])
    if normalize(low) != normalize(high):
        raise ValueError("Reference refinement changed more than the sample budget")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for label in PROVIDERS:
        parser.add_argument(f"--{label.replace('_', '-')}", type=Path, required=True)
    parser.add_argument("--scene", choices=SCENES, required=True)
    parser.add_argument("--regions", type=Path, help="JSON object of named normalized xyxy boxes")
    parser.add_argument("--reference-mae-limit", type=float, required=True,
                        help="predeclared cap on Reference low-vs-high mean absolute RGB8 per region")
    args = parser.parse_args()
    try:
        if not math.isfinite(args.reference_mae_limit) or not 0 <= args.reference_mae_limit <= 255:
            raise ValueError("Reference MAE limit must be in [0, 255]")
        custom = json.loads(args.regions.read_text(encoding="utf-8")) if args.regions else None
        metas = {label: metadata(getattr(args, label).with_suffix(".coverage.txt")) for label in PROVIDERS}
        validate_temporal(metas, args.scene)
        images = {label: verified_image(getattr(args, label), meta) for label, meta in metas.items()}
        pairs = [("reference_low", "reference"), ("off", "reference")]
        pairs += [(sdk, control) for sdk in ("fsr", "rr") for control in ("off", "reference")]
        comparisons = {f"{a}-vs-{b}": compare(images[a], images[b], custom)[0] for a, b in pairs}
        bad = [k for k, m in comparisons["reference_low-vs-reference"].items()
               if m["mean_abs_rgb8"] > args.reference_mae_limit]
    except (KeyError, OSError, TypeError, ValueError) as exc:
        parser.error(str(exc))
    print(json.dumps({"scene": args.scene, "mode": metas["off"]["mode"], "adapter": metas["off"]["adapter"],
                      "evidence_scope": "sidecar consistency and encoded RGB8 differences; verify physical dispatch separately",
                      "captures": {label: {"stem": str(getattr(args, label)), "meta": meta} for label, meta in metas.items()},
                      "reference_mae_limit": args.reference_mae_limit, "reference_refinement_failed_regions": bad,
                      "comparisons": comparisons}, indent=2))
    return int(bool(bad))


if __name__ == "__main__":
    raise SystemExit(main())
