#!/usr/bin/env python3
"""Capture matched retail cameras; never substitute a CPU driver for Hardware.

Requires Pillow for PNG decoding and region statistics. Client bytes are not
copied into the output, only per-frame images, sidecars, logs and the report.
"""

import argparse
import csv
import hashlib
import io
from itertools import combinations
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import statistics
import subprocess
from datetime import datetime, timezone

from PIL import Image, ImageChops

from lighter_host_evidence import requested_target


PROFILES = {
    "cpu": ("off", "software"),
    "physical": ("off", "software", "hardware"),
}
REUSE = {"off": 0, "di": 1, "gi": 2, "both": 3}
CLASSES = {"city", "forest", "open_distance", "water", "cave_instance", "effects"}
CONDITIONS = {"day": (12, "Clear"), "night": (0, "Clear"), "rain": (12, "Rain")}
NATIVE_SCOPE = "main_streamed_v1"
SHARED_INPUTS = {"textures.pck", "building.pck", "litmodels.pck", "gfx.pck", "surfaces.pck",
                 "configs.pck", "grasses.pck", "models.pck"}
SKIP_REASONS = ("range", "cutoff", "replaced", "budget", "pending", "disabled", "empty")
FAIL_REASONS = ("package", "unavailable", "invalid", "unsupported")
TOTALS = ("loaded", "skipped", "failed")
INVENTORY_COLUMNS = ("class", *TOTALS, *SKIP_REASONS, *FAIL_REASONS, "texture_fallback")
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


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def verify_inputs(client, manifest):
    """Verify immutable reference pins; input files never enter the report."""
    root = (client / "element" if (client / "element" / "maps").is_dir() else client).resolve()
    pins = {}
    for number, line in enumerate(manifest.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        # Either sha256sum output or the checked-in Drive-id/SHA/path pins.
        fields = line.split()
        if len(fields) not in (2, 3):
            raise ValueError(f"{manifest}:{number}: expected SHA-256 and element-relative path")
        digest, name = fields[-2:]
        relative = PurePosixPath(name)
        if (not re.fullmatch(r"[0-9a-f]{64}", digest) or not re.fullmatch(r"[a-zA-Z0-9_./-]+", name)
                or relative.is_absolute() or ".." in relative.parts or relative.as_posix() != name
                or name in pins):
            raise ValueError(f"{manifest}:{number}: invalid or duplicate input pin")
        path = root / relative
        if sha256_file(path) != digest:
            raise ValueError(f"input SHA-256 mismatch: {name}")
        pins[name] = digest
    if not pins:
        raise ValueError("empty input manifest")
    return pins


def case_has_input_pins(case, pins):
    # A gate on the shared archive set and core selected-map records, not
    # a claim that unlisted optional members/add-ons were present.
    prefix = f"maps/{case['map']}/"
    stem = prefix + case["map"]
    members = {stem + "." + ext for ext in ("ecwld", "ecbsd", "trn2")}
    block_exts = {PurePosixPath(name).suffix for name in pins if name.startswith(prefix)}
    return SHARED_INPUTS | members <= pins.keys() and {".t2bk", ".t2mk"} <= block_exts


def validate_cases(cases):
    ids = set()
    for case in cases:
        name = case["id"]
        if not re.fullmatch(r"[a-z0-9-]+", name) or name in ids:
            raise ValueError(f"invalid or duplicate case ID {name!r}")
        ids.add(name)
        pose = case.get("camera")
        if (not re.fullmatch(r"[a-z0-9_-]+", case["map"]) or not set(case["classes"]) <= CLASSES
                or case["condition"] not in CONDITIONS or not isinstance(case["lightmaps"], bool)
                or type(case["warmup"]) is not int or not 0 <= case["warmup"] <= 120
                or (pose is not None and (len(pose) != 5 or not all(math.isfinite(x) for x in pose)))):
            raise ValueError(f"invalid case {name}")
    if not ids:
        raise ValueError("expected nonempty capture manifest")


def metadata(path):
    fields = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if "=" in line:
            name, value = line.split("=", 1)
            if name in fields:
                raise ValueError(f"{path}: duplicate field {name}")
            fields[name] = value
    schema = fields.get("coverage_schema", "")
    if not schema.isdecimal() or int(schema) < 3:
        raise ValueError(f"{path}: expected matched capture schema 3 or newer")
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
        channel_max = [max((i for i, n in enumerate(hist[j:j + 256]) if n), default=0) for j in (0, 256, 512)]
        summary[name] = {
            "box_xyxy": box,
            "pixels": pixels,
            "mean_abs_rgb8": round(statistics.mean(channel_mae), 5),
            "channel_mean_abs_rgb8": [round(v, 5) for v in channel_mae],
            "max_abs_rgb8": max(channel_max),
            "channel_max_abs_rgb8": channel_max,
        }
    return summary, diff


def validate_lightmap_admission(meta, mode, case, capture_name):
    minimum = case.get("min_lightmap_areas_day_night", 0)
    if minimum and (not case["lightmaps"]
                    or int(meta.get("native_lightmap_areas_day_night", 0)) < minimum
                    or (mode != "off" and int(meta.get("ray_lightmap_areas_bound", 0)) < minimum)):
        raise ValueError(f"{capture_name}: need at least {minimum} bound authored day/night lightmap areas")


def capture_metadata(stem, mode, case, profile):
    meta = metadata(stem.with_suffix(".coverage.txt"))
    wet_depth = float(meta.get("eye_underwater_depth", "0"))
    if not math.isfinite(wet_depth) or wet_depth < 0:
        raise ValueError(f"{stem.name}: invalid eye underwater depth")
    if "eye_underwater" in case and (wet_depth > 0.) != case["eye_underwater"]:
        raise ValueError(f"{stem.name}: wrong side of the authored water surface")
    if (json.loads(meta["map"]) != case["map"] or meta["mode"].lower() != mode
            or (float(meta["hour"]), meta["weather"]) != CONDITIONS[case["condition"]]
            or int(meta["warmup_frames"]) != case["warmup"]
            or int(meta["warmup_step_ms"]) != 16
            or int(meta["animation_elapsed_ms"]) != 16 * case["warmup"]
            or (meta["rain_at_eye"] == "true") != (case["condition"] == "rain" and wet_depth == 0.)
            or (meta["lightmaps"] == "true") != case["lightmaps"]):
        raise ValueError(f"{stem.name}: capture doesn't match the case")
    if meta.get("ray_reconstruction", "portable") != "portable":
        raise ValueError(f"{stem.name}: matrix requires portable ray reconstruction")
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
    validate_lightmap_admission(meta, mode, case, stem.name)
    return meta, verified_image(stem, meta)


def verified_image(stem, meta):
    with Image.open(stem.with_suffix(".png")) as source:
        image = source.convert("RGBA")
    if f"{image.width}x{image.height}" != meta["image"]:
        raise ValueError(f"{stem.name}: extent disagrees with metadata")
    if hashlib.sha256(image.tobytes()).hexdigest() != meta["rgba8_sha256"]:
        raise ValueError(f"{stem.name}: decoded RGBA8 hash disagrees with metadata")
    return image


def inventory_for(stem, case, expected_digest=None):
    raw = stem.with_suffix(".coverage.csv").read_bytes()
    if expected_digest is not None and hashlib.sha256(raw).hexdigest() != expected_digest:
        raise ValueError(f"{stem.name}: inventory SHA-256 disagrees with metadata")
    rows = {}
    reader = csv.DictReader(io.StringIO(raw.decode("utf-8"), newline=""))
    if tuple(reader.fieldnames or ()) != INVENTORY_COLUMNS:
        raise ValueError(f"{stem.name}: invalid inventory columns")
    for row in reader:
        cls = row.pop("class")
        if cls in rows:
            raise ValueError(f"{stem.name}: duplicate class {cls}")
        if None in row or any(value is None for value in row.values()):
            raise ValueError(f"{stem.name}: invalid inventory row {cls}")
        rows[cls] = {key: int(value) for key, value in row.items()}
    required = {"near_terrain", "far_terrain", "trees", "grass", "buildings", "water",
                "world_models", "walker", "populator", "effect_draws", "effect_roots"}
    if missing := required - rows.keys():
        raise ValueError(f"{stem.name}: missing inventory rows {sorted(missing)}")
    for cls, row in rows.items():
        skipped = sum(row[k] for k in SKIP_REASONS)
        failed = sum(row[k] for k in FAIL_REASONS)
        if any(v < 0 for v in row.values()) or row["skipped"] != skipped or row["failed"] != failed:
            raise ValueError(f"{stem.name}: {cls} counts do not match exclusive reasons")
    if rows["near_terrain"]["loaded"] <= 0 and rows["far_terrain"]["loaded"] <= 0:
        raise ValueError(f"{stem.name}: no terrain in the admitted scene")
    for cls, expected in case.get("authored_totals", {}).items():
        total = sum(rows[cls][key] for key in TOTALS)
        if total != expected:
            raise ValueError(f"{stem.name}: {cls} accounted for {total}, not {expected}")
    for cls, expected in case.get("min_loaded", {}).items():
        if rows[cls]["loaded"] < expected:
            raise ValueError(f"{stem.name}: {cls} loaded {rows[cls]['loaded']}, expected at least {expected}")
    return rows


def capture_inventory(stem, mode, meta, case):
    digest = meta.get("inventory_sha256")
    native = mode == "off"
    if native and meta.get("native_inventory_scope") != NATIVE_SCOPE:
        return None   # Historical submission counters are not a census.
    if native and not digest:
        raise ValueError(f"{stem.name}: missing Native inventory SHA-256")
    totals = dict(case.get("authored_totals", {}))
    # Authored manifest counts describe direct scene roots only. The CSV
    # additionally owns model event descriptors, including rejected hooks.
    direct_key = "world_effect_authored_scene_roots"
    if direct_key in meta and (direct := totals.pop("effect_roots", None)) is not None and int(meta[direct_key]) != direct:
        raise ValueError(f"{stem.name}: expected {direct} authored scene effect roots")
    rows = inventory_for(stem, {
        "authored_totals": totals,
        "min_loaded": case.get("native_min_loaded" if native else "min_loaded", {}),
    }, digest)
    if direct_key in meta:
        root_count = int(meta[direct_key]) + int(meta["world_effect_decoded_model_event_roots"])
        if sum(rows["effect_roots"][k] for k in TOTALS) != root_count:
            raise ValueError(f"{stem.name}: recognized effect root counts do not reconcile")
    if not native:
        return rows
    expected = {"buildings": "native_authored_bmds", "water": "native_authored_water_areas",
                "trees": "native_authored_tree_placements", "world_models": "native_authored_world_actors",
                "effect_draws": "native_effect_quads"}
    for cls, key in expected.items():
        if sum(rows[cls][k] for k in TOTALS) != int(meta[key]):
            raise ValueError(f"{stem.name}: Native {cls} does not reconcile with {key}")
    area_keys = ("range", "empty", "cutoff", "budget", "disabled", "unavailable", "loaded", "skipped", "failed")
    areas = {key: int(meta[f"grass_areas_{key}"]) for key in area_keys}
    if (meta.get("grass_inventory_scope") != "native_stream_only"
            or any(value < 0 for value in areas.values())
            or sum(areas[k] for k in TOTALS) != int(meta["grass_authored_areas"])
            or areas["skipped"] != sum(areas[k] for k in area_keys[:5])
            or areas["failed"] != areas["unavailable"]):
        raise ValueError(f"{stem.name}: Native grass area counts do not reconcile")
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


def run_case(case, args, attempts):
    name = case["id"]
    profile = args.profile
    mode_results, images = {}, {}
    variants = PROFILES[profile] + (("hybrid",) if args.hybrid_ab else ())
    for variant in variants:
        raster = variant == "hybrid"
        mode = "hardware" if raster else variant
        stem = args.output / f"{name}-{variant}"
        hour, weather = CONDITIONS[case["condition"]]
        env = {k: v for k, v in os.environ.items() if not k.startswith("PW_PREVIEW_")}
        for key in ("PW_TEST_NO_LIGHTMAPS", "PW_RAY_COMPACT_TERRAIN", "PW_RAY_BC_TEXTURES",
                    "PW_RAY_SCREEN_REUSE", "PW_RAY_RECONSTRUCTION", "PW_UI_TEST", "PW_SKIP_TREES_TEST",
                    "PW_DISABLE_WORLD_MODELS"):
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
        if profile == "physical" and args.device and f'name: "{args.device}"' not in meta["adapter"]:
            raise ValueError(f"{stem.name}: capture did not use {args.device}")
        if mode != "off":
            width, height = [int(n) for n in meta["image"].split("x")]
            expected = f"{width * args.ray_resolution // 100}x{height * args.ray_resolution // 100}"
            if meta.get("trace_image") != expected:
                raise ValueError(f"{stem.name}: expected actual ray extent {expected}, got {meta.get('trace_image')}")
        if meta.get("raster_primary", "false") != str(raster).lower():
            raise ValueError(f"{stem.name}: requested and recorded primary path differ")
        requested_reuse = REUSE[args.screen_reuse] if mode != "off" else 0
        if (int(meta.get("screen_reuse_flags", "0")) & 3) != requested_reuse:
            raise ValueError(f"{stem.name}: requested screen reuse not admitted")
        rows = capture_inventory(stem, mode, meta, case)
        paths = [stem.with_suffix(ext) for ext in (".png", ".coverage.txt", ".coverage.csv", ".log")]
        attempts[variant] = {"files": stem.name, "metadata": meta, "inventory_complete": rows is not None,
                             "sha256": {p.name: sha256_file(p) for p in paths if p.is_file()}}
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


def summarize_matrix(report, output):
    """Small index of *validated* pairs, distinct from their full artifacts."""
    maps = {}
    adapters = set()
    clear = {}
    for result in report["cases"]:
        case = result["case"]
        for route, capture in result["captures"].items():
            if case["condition"] == "day":
                # Two day cameras in the same map need not share a baseline.
                clear[route, case["map"], capture["meta"]["eye"], capture["meta"]["yaw"], capture["meta"]["pitch"]] = capture
    for result in report["cases"]:
        case = result["case"]
        row = {"id": case["id"], "routes": {}, "comparisons": {}}
        for route, capture in result["captures"].items():
            meta = capture["meta"]
            adapters.add(meta["adapter"])
            stats = {"image": meta["image"], "trace_image": meta["trace_image"],
                     "eye_depth": float(meta["eye_underwater_depth"]), "rain_mask": meta.get("rain_mask_source", "unknown"),
                     "rain_count": (int(meta["native_rain_slots_selected"]) if "native_rain_slots_selected" in meta else None)
                     if route == "off" else int(meta["ray_rain_cards"]),
                     "lightmap_pairs": int(meta["native_lightmap_areas_day_night"]),
                     "ray_lightmap_bound": int(meta.get("ray_lightmap_areas_bound", 0)),
                     "warp_omitted": int(meta["world_effect_warp_omitted_elements"]),
                     "dst_color_candidates": [int(meta[f"world_effect_dest_color_{key}_candidates"])
                                              for key in ("affine", "unsupported")],
                     "effect_event_roots": int(meta.get("world_effect_decoded_model_event_roots", 0))}
            if route == "off" and "native_last_scene_passes" in meta:
                stats["native_scene_passes"] = int(meta["native_last_scene_passes"])
            if capture["inventory"] is not None:
                for cls in ("buildings", "water", "grass", "effect_roots", "effect_draws"):
                    stats[cls] = {k: v for k, v in capture["inventory"][cls].items() if v != 0}
            timing = capture["synchronized_capture_ms"]
            if timing is not None:
                stats["sync_count_p50_p95"] = [len(timing["samples"]), round(timing["p50"], 3), round(timing["p95"], 3)]
            reference = clear.get((route, case["map"], meta["eye"], meta["yaw"], meta["pitch"]))
            if case["condition"] == "rain" and reference is not None:
                ref_meta = reference["meta"]
                controls = tuple(k for k in MATCH_KEYS if k not in ("weather", "rain_at_eye")) + (
                    "settings_effective", "eye_underwater_depth", "rain_mask_source", "trace_image",
                )
                if all(ref_meta.get(k) == meta.get(k) for k in controls):
                    dry = verified_image(output / reference["files"], ref_meta)
                    rain = verified_image(output / capture["files"], meta)
                    delta = ImageChops.difference(dry.convert("RGB"), rain.convert("RGB"))
                    stats["clear_to_rain_changed_pixels"] = sum(px != (0, 0, 0) for px in delta.getdata())
            row["routes"][route] = stats
        for pair, diff in result["comparisons"].items():
            row["comparisons"][pair] = {region: [d["mean_abs_rgb8"], d["max_abs_rgb8"]] for region, d in diff.items()}
        maps.setdefault(case["map"], []).append(row)
    return {"source_sha": report["source_sha"], "profile": report["profile"], "adapters": sorted(adapters),
            "matched_cases": len(report["cases"]), "failed_cases": sorted(report["errors"]),
            "missing_physical_cells": len(report["missing_rt17_cells"]), "maps": maps}


def missing_rt17_cells(cases, report):
    matched = {result["case"]["id"]: result["captures"] for result in report["cases"]}
    missing = []
    for subject in sorted(CLASSES):
        for condition in CONDITIONS:
            declared = [c for c in cases if subject in c["classes"] and c["condition"] == condition]
            for route in PROFILES["physical"]:
                if report["profile"] != "physical":
                    reason = "CPU subset is not a physical renderer sample"
                elif not declared:
                    reason = "no camera declared for this scene class and condition"
                else:
                    reason = "no matched capture"
                    passed = False
                    for c in declared:
                        capture = matched.get(c["id"], {}).get(route)
                        if capture is None:
                            error = report["errors"].get(c["id"])
                            reason = f"{c['id']}: {error}" if error else reason
                        elif capture["inventory"] is None:
                            reason = f"{c['id']}: full Native loaded/skipped/failed inventory not recorded"
                        elif not requested_target(capture["meta"]):
                            reason = f"{c['id']}: not the requested Windows x64 RTX 3080 10 GB Vulkan target"
                        elif route != "off" and capture["meta"].get("trace_image") != "480x360":
                            reason = f"{c['id']}: not the required actual 50% ray extent"
                        elif not c.get("camera") or not c.get("camera_evidence"):
                            reason = f"{c['id']}: no reviewed explicit camera and evidence reference"
                        elif not case_has_input_pins(c, report.get("client_inputs", {})):
                            reason = f"{c['id']}: input pins for the map and shared archive set were not verified"
                        elif not capture["meta"].get("inventory_sha256"):
                            reason = f"{c['id']}: inventory is not bound to the pictured frame sidecar"
                        elif not capture["synchronized_capture_ms"] or len(capture["synchronized_capture_ms"]["samples"]) < 3:
                            reason = f"{c['id']}: fewer than three synchronized draws"
                        else:
                            passed = True
                    if passed:
                        continue
                missing.append({"scene_class": subject, "condition": condition, "route": route, "reason": reason})
    return missing


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("manifest", "viewer", "client", "output"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    parser.add_argument("--input-manifest", type=Path, help="trusted SHA-256 pins for the client subset under element/")
    parser.add_argument("--profile", choices=PROFILES, default="physical")
    parser.add_argument("--ray-resolution", type=int, default=50, choices=range(25, 101), metavar="25..100")
    parser.add_argument("--timed-frames", type=int, choices=range(121), default=0, metavar="0..120")
    parser.add_argument("--timeout", type=int, default=360, help="seconds per capture")
    parser.add_argument("--device", help="require the recorded Vulkan adapter's exact name for physical runs")
    parser.add_argument("--hybrid-ab", action="store_true", help="also capture Hardware with rasterized 1-spp primary visibility")
    parser.add_argument("--compact-terrain", action="store_true", help="use the experimental buffer-backed BC terrain path")
    parser.add_argument("--fsr1-easu", action="store_true", help="use the spatial EASU color resolve on the ray modes")
    parser.add_argument("--progressive", action="store_true", help="accumulate ray history without requiring a reuse cache")
    parser.add_argument("--screen-reuse", choices=REUSE, default="off", help="experimental 1-spp reuse; enables progressive history")
    parser.add_argument("--require-complete", action="store_true", help="fail unless all required physical matrix cells exist")
    args = parser.parse_args()
    if args.hybrid_ab and args.profile != "physical":
        parser.error("--hybrid-ab requires --profile physical")
    try:
        cases = json.loads(args.manifest.read_text(encoding="utf-8"))["cases"]
        validate_cases(cases)
        pins = verify_inputs(args.client, args.input_manifest) if args.input_manifest else {}
        viewer_hash = sha256_file(args.viewer)
    except (ValueError, KeyError, TypeError, OSError) as error:
        parser.error(str(error))
    covered = {(cls, case["condition"]) for case in cases for cls in case["classes"]}
    missing = sorted({(cls, condition) for cls in CLASSES for condition in CONDITIONS} - covered)
    complete = not missing and args.profile == "physical"
    if args.require_complete:
        if args.profile != "physical":
            parser.error("--require-complete needs --profile physical")
        if args.timed_frames < 3 or args.ray_resolution != 50:
            parser.error("complete physical runs need at least three timed frames per mode and 50% ray resolution")
    args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()):
        parser.error(f"use a fresh output directory: {args.output}")
    report = {"started_utc": datetime.now(timezone.utc).isoformat(), "source_sha": os.environ.get("GITHUB_SHA"),
              "profile": args.profile, "manifest_sha256": sha256_file(args.manifest),
              "viewer_sha256": viewer_hash, "client_inputs": pins, "options": {"compact_terrain": args.compact_terrain,
                                                     "fsr1_easu": args.fsr1_easu, "screen_reuse": args.screen_reuse,
                                                     "progressive": args.progressive or args.screen_reuse != "off"},
              "declared_physical_matrix_complete": complete, "missing_scene_conditions": missing,
              "metric": "mean absolute difference of encoded RGB8 pixel channels (lower is closer, not a pass threshold)",
              "cases": []}
    report["errors"] = {}
    report["capture_attempts"] = {}
    # Print details on every pair to the log. CI separately transports a
    # compact per-map index since annotations have a per-step count limit.
    for case in cases:
        attempts = report["capture_attempts"][case["id"]] = {}
        try:
            result = run_case(case, args, attempts)
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
        native = result["captures"]["off"]
        if native["inventory"] is not None:
            nb, nr = (native["inventory"][k] for k in ("buildings", "effect_roots"))
            listed = lambda row: "/".join(str(row[k]) for k in TOTALS)
            print(f"{case['id']} Native BMD {listed(nb)}; GFX roots {listed(nr)}; "
                  f"water depth {native['meta']['eye_underwater_depth']}; "
                  f"rain mask authored={native['meta']['rain_mask_authored']}, "
                  f"native slots={native['meta']['native_rain_slots_selected']}, "
                  f"ray cards={software['meta']['ray_rain_cards']}", flush=True)
        print(f"{case['id']} RGB8 MAE {summary}; ray BMD {bmd['loaded']}/{bmd['skipped']}/{bmd['failed']}; "
              f"GFX roots {roots['loaded']}/{roots['skipped']}/{roots['failed']}; "
              f"eye {software['meta']['eye']} yaw {software['meta']['yaw']} pitch {software['meta']['pitch']}; "
              f"{timing or 'no synchronized timing requested'}", flush=True)
        if len(report["cases"]) == 1:
            print(software["meta"]["adapter"], flush=True)
    report["missing_rt17_cells"] = missing_rt17_cells(cases, report)
    report["declared_physical_matrix_complete"] = (complete and not report["errors"] and not report["missing_rt17_cells"]
                                                    and args.ray_resolution == 50)
    (args.output / "matrix.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    (args.output / "capture-summary.json").write_text(
        json.dumps(summarize_matrix(report, args.output), indent=2) + "\n", encoding="utf-8")
    (args.output / "SHA256SUMS.txt").write_text(
        "".join(f"{sha256_file(path)}  {path.name}\n" for path in sorted(args.output.iterdir()) if path.is_file()),
        encoding="utf-8", newline="\n",
    )
    if report["errors"]:
        message = f"Failed capture cases: {', '.join(report['errors'])}; retained partial evidence in matrix.json"
        print(f"{'::error::' if os.environ.get('GITHUB_ACTIONS') == 'true' else ''}{message}", flush=True)
        raise SystemExit(1)
    if args.require_complete and not report["declared_physical_matrix_complete"]:
        raise SystemExit(f"RT-17 remains incomplete; see {args.output / 'matrix.json'} for per-route reasons")


if __name__ == "__main__":
    main()
