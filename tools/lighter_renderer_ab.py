#!/usr/bin/env python3
"""Review a matched portable Hardware experiment in counterbalanced A-B-B-A order."""

import argparse
import hashlib
import json
import math
from pathlib import Path

from lighter_host_evidence import requested_target
from lighter_travel_report import durations, read_csv, receipt_summary, validate_pair, validate_rows

TEXTURES = ("ray_texture_bank_bytes", "ray_bc_tiles", "ray_bc5_tiles", "ray_bc7_tiles", "ray_bc_image_allocated_bytes")
PACING = ("resident_copy_batches", "resident_host_bytes", "resident_reused_bytes", "resident_max_batch_bytes",
          "resident_blas_batches", "resident_blas_builds")
TREATMENT = (False, True, True, False)


def renderer_counters(rows, sampled, materials, require_pacing):
    textures, commits = [], []
    for row in rows:
        bank, tiles, bc5, bc7, images = values = tuple(int(row[k]) for k in TEXTURES)
        if bank <= 0 or min(tiles, bc5, bc7, images) < 0 or bc5 + bc7 > tiles:
            raise ValueError("invalid texture receipt")
        if (sampled and (not tiles or not images)) or (not sampled and any(values[1:])):
            raise ValueError("sampled BC was not admitted as requested or allocations survived in the control")
        textures.append(values)
        pacing = [row[k] for k in PACING]
        if any(pacing):
            if not all(pacing) or row["committed"] != "1":
                raise ValueError("pacing receipt must describe a completed publication")
            n, host, reused, peak, batches, builds = counters = tuple(map(int, pacing))
            if min(counters) < 0 or not 0 < peak <= min(host + reused, 4 << 20) or host + reused > n * peak:
                raise ValueError("invalid or over-budget committed copy receipt")
            if not batches <= builds <= 8 * batches:
                raise ValueError("invalid committed BLAS batch receipt")
            commits.append({"frame": int(row["frame"]), **dict(zip(PACING, counters, strict=True))})
    if materials and sampled and any(not (v[2] and v[3]) for v in textures):
        raise ValueError("both BC5 and BC7 sampled pages must be admitted for the requested material comparison")
    if require_pacing and not any(c["resident_blas_builds"] for c in commits):
        raise ValueError("no measured, paced publication with BLAS work; scout a resident crossing")
    return {"resident_commit_receipts": commits,
            "texture_counters_peak": {key: max(v[i] for v in textures) for i, key in enumerate(TEXTURES)},
            "texture_counters_min": {key: min(v[i] for v in textures) for i, key in enumerate(TEXTURES)}}


def load(path, sync, kind, treatment, materials=False, require_pacing=False):
    meta, rows = read_csv(path)
    record = validate_rows(path, meta, rows, "synchronized" if sync else "normal_present")
    if not requested_target(meta) or meta.get("ray_reconstruction") != "portable":
        raise ValueError(f"{path}: expected portable Windows x64 RTX 3080 10 GB Vulkan evidence")
    hybrid = kind == "hybrid" and treatment
    sampled = kind == "sampled-bc" and treatment
    if (record[1] != ("Hybrid" if hybrid else "Hardware") or meta["sampled_bc"] != str(sampled).lower()
            or meta["compact_terrain"] != str(kind == "sampled-bc").lower() or meta["screen_cache_mask"] != "0"):
        raise ValueError(f"{path}: wrong Hardware/Hybrid/BC control or unexpected reuse")
    summary = receipt_summary(path, record, require_heap=True)
    if not summary["peak_sampled_device_local_heap_estimate_bytes"] or (sync and any(not r[4] for r in record[3])):
        raise ValueError(f"{path}: missing process driver-heap samples")
    summary |= renderer_counters(rows, sampled, materials, require_pacing)
    if sync:
        primary_trace = []
        for r in record[3]:
            _, build, raster, trace, reproject, filt, composite, sdk, native = r[7]
            if (trace is None or trace <= 0 or any(x is None for x in (build, reproject, filt, composite))
                    or sdk is not None or native is not None or (hybrid and (raster is None or raster <= 0))
                    or (not hybrid and raster is not None)):
                raise ValueError(f"{path}: missing or mismatched current-frame stages (include raster in Hybrid)")
            primary_trace.append((raster or 0.) + trace)
        summary["gpu_primary_plus_trace"] = durations(primary_trace)
    return record, summary


def review(kind, normal, sync, minimum_gain):
    baseline = normal[0][0]
    for record, _ in (*normal, *sync):
        validate_pair(baseline, record, sampled_bc_ab=kind == "sampled-bc")
    repetitions = []
    for a, b in ((0, 1), (3, 2)):
        an, bn, ag, bg = (normal[a][1], normal[b][1], sync[a][1], sync[b][1])
        gain = lambda base, candidate: 100 * (1 - candidate / base)
        tail = gain(an["normal_present_calls"]["p95_ms"], bn["normal_present_calls"]["p95_ms"])
        trace = gain(ag["gpu_primary_plus_trace"]["p50_ms"], bg["gpu_primary_plus_trace"]["p50_ms"])
        repetitions.append({"indices": [a, b], "normal_p95_gain_percent": tail, "gpu_primary_plus_trace_p50_gain_percent": trace,
                            "sampled_peak_heap_delta_bytes": bg["peak_sampled_device_local_heap_estimate_bytes"] - ag["peak_sampled_device_local_heap_estimate_bytes"],
                            "meets_timing_threshold": min(tail, trace) >= minimum_gain})
    return {"experiment": kind, "minimum_gain_percent": minimum_gain, "comparisons": repetitions,
            "repeatable_timing_gain_only": all(r["meets_timing_threshold"] for r in repetitions),
            "scope": "receipt comparison; retain actual workload, visuals, input pins and allocation review separately; no default change",
            "normal_present": [r[1] for r in normal], "synchronized": [r[1] for r in sync]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kind", choices=("hybrid", "sampled-bc"), required=True)
    parser.add_argument("--normal", type=Path, nargs=4, required=True, metavar=("A1", "B1", "B2", "A2"))
    parser.add_argument("--sync", type=Path, nargs=4, required=True, metavar=("A1", "B1", "B2", "A2"))
    parser.add_argument("--require-bc5-bc7", action="store_true", help="require both sampled material formats in B")
    parser.add_argument("--require-pacing", action="store_true", help="require a measured staged publication with BLAS builds in every run")
    parser.add_argument("--minimum-gain-percent", type=float, required=True, help="predeclared minimum gain required in both repetitions")
    args = parser.parse_args()
    try:
        if not math.isfinite(args.minimum_gain_percent) or not 0 < args.minimum_gain_percent < 100:
            raise ValueError("minimum gain must be positive and below 100 percent")
        if args.require_bc5_bc7 and args.kind != "sampled-bc":
            raise ValueError("per-format admission requires --kind sampled-bc")
        paths = [p.resolve() for p in (*args.normal, *args.sync)]
        if len(set(paths)) != 8 or len({hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}) != 8:
            raise ValueError("use eight distinct, independent CSV runs, not copies")
        normal, sync = ([(load(path, policy, args.kind, b, args.require_bc5_bc7, args.require_pacing))
                         for path, b in zip(source, TREATMENT, strict=True)]
                        for policy, source in ((False, args.normal), (True, args.sync)))
        result = review(args.kind, normal, sync, args.minimum_gain_percent)
    except (KeyError, OSError, TypeError, ValueError) as error:
        parser.error(str(error))
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
