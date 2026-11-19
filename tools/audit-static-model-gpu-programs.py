#!/usr/bin/env python3
"""Fail closed if any static-model GPU diagnostic is missing from CI.

The manifest names the diagnostics that exercise static BMD scenery (or the
production world WGSL / model texture stages). Every other GPU diagnostic is
listed with an explicit scope reason so an omission cannot hide in the set.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "tools/static-model-gpu-programs.txt"
WORKFLOW = ROOT / ".github/workflows/pr-checks.yml"

EXCLUDED = {
    "bloom-gpu-check": "post-processing glow chain, not a static-model pipeline",
    "display-gamma-gpu-check": "display gamma ramp, not a static-model pipeline",
    "post-gpu-check": "fullscreen post chain, not a static-model pipeline",
    "model-draw-gpu-check": "animated/world EC model instance pipeline, not static BMD scenery",
    "model-light-gpu-check": "standalone animated model light helper",
    "model-receiver-gpu-check": "posed animated receiving-model pipeline",
    "simple-water-gpu-check": "water surface material",
    "skin-palette-gpu-check": "animated skin palette",
    "skin-pose-gpu-check": "animated skin pose",
    "terrain-lod-gpu-check": "terrain LOD",
    "terrain-view-gpu-check": "terrain per-view pass state / LOD continuity, not static BMD scenery",
    "terrain-refraction-gpu-check": "terrain refraction/fog, not static BMD",
    "tree-gpu-mip-check": "asset-dependent viewer check requires user trees.pck and baked PWTREE cache; the synthetic authored-chain check is wired in CI",
    "water-fog-gpu-check": "water fog",
    "water-pass-gpu-check": "overlapping water-plane depth ordering, not static BMD",

}

def main() -> int:
    names = [
        line.strip()
        for line in MANIFEST.read_text().splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]
    errors = []
    workflow = WORKFLOW.read_text()
    for name in names:
        if not (ROOT / "src/bin" / f"{name}.rs").is_file():
            errors.append(f"missing diagnostic source: {name}")
        # Two accepted shapes: a direct `cargo run --locked --bin NAME`, or the
        # `run_check NAME` helper the shader step uses so a failure names the
        # binary. Both are literal names, so this stays an exact check.
        wired = re.search(
            rf"cargo run --locked --bin {re.escape(name)}\b", workflow
        ) or re.search(rf"^\s*run_check {re.escape(name)}\s*$", workflow, re.M)
        if not wired:
            errors.append(f"not wired into CI: {name}")
    gpu_bins = sorted(p.stem for p in (ROOT / "src/bin").glob("*gpu*.rs"))
    excluded = [name for name in gpu_bins if name not in names]
    for name in excluded:
        reason = EXCLUDED.get(name)
        if not reason:
            errors.append(f"excluded diagnostic has no named reason: {name}")
        else:
            print(f"EXCLUDED {name}: {reason}")
    for name, reason in EXCLUDED.items():
        if name not in excluded:
            errors.append(f"stale exclusion reason: {name}")
    for name in names:
        print(f"WIRED {name}")
    print(
        f"OK: {len(names)} static-model GPU diagnostics wired; "
        f"{len(excluded)} excluded with reasons; {len(gpu_bins)} GPU diagnostics total"
    )
    for error in errors:
        print(f"ERROR {error}", file=sys.stderr)
    return 1 if errors else 0

if __name__ == "__main__":
    raise SystemExit(main())
