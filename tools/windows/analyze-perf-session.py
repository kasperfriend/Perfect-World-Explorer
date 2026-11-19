#!/usr/bin/env python3
"""Analyze a Windows Lighter performance bundle without running any payload."""
from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import math
import os
import re
import shutil
import stat
import sys
import zipfile
from datetime import datetime, timedelta, timezone
from pathlib import Path, PurePosixPath
from urllib.parse import quote

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import lighter_travel_report as travel
from lighter_host_evidence import TARGET_ADAPTER, requested_target

ROOT = Path(__file__).resolve().parents[2]
ROUTES = {
    "native": ("Off", "portable", False),
    "software": ("Software", "portable", False),
    "hardware": ("Hardware", "portable", False),
    "hybrid": ("Hybrid", "portable", False),
    "hardware-rr": ("Hardware", "rr", False),
    "software-fsr": ("Software", "fsr", False),
    "candidate": ("Hybrid", "portable", True),
    "native-dx12": ("Off", "portable", False),
}
LABEL = {"portable": "portable", "rr": travel.PROVIDERS[2], "fsr": travel.PROVIDERS[1]}
POLICIES = ("normal_present", "synchronized")
GAME_FLAGS = {"PW_RAY_GAME_PROFILE", "PW_RAY_PACED_STARTUP", "PW_RAY_EXACT_PROXY",
              "PW_RAY_COMPACT_BLAS", "PW_RAY_BLUE_NOISE", "PW_RAY_BC_TEXTURES"}
PINS = {
    "element": "a290998dc13cc5558b54bda3f65d099f95bc0702e54a0613c6d7669f35c6cdb0",
    "fsr": "4a7e895af6e0b615d40a84d59eb8c2b1054ad91cdd0c739567940270d51bdab7",
    "rr_feature": "b49435ae346f227feafe7bbe937d88277ea0ee7323975ec06488f377fdb46cee",
    "rr_loader_lib": "7b9a5239c1145af5f645ab472aa066f9ea79db36ac819562312aae69d65f3765",
}
SHA_RE = re.compile(r"[0-9a-f]{64}\Z")
PROBLEM = re.compile(r"ERROR_DEVICE_LOST|Parent device is lost|Graphics device lost|"
                     r"0xc0000005|thread .* panicked", re.I)
MAX_TOTAL = 512 * 2**20
MAX_MEMBER = 64 * 2**20


def require(ok, message):
    if not ok:
        raise ValueError(message)


def safe_name(name):
    # Safe on Windows too: no NTFS streams, device names, case aliases,
    # alternate separators, trailing periods/spaces, or directory traversal.
    parts = name.split("/")
    require(all(re.fullmatch(r"[A-Za-z0-9_.-]+", p) and p not in (".", "..")
                and not p.endswith(".") and not re.fullmatch(
                    r"(?:AUX|PRN|CON|NUL|COM[1-9]|LPT[1-9])(?:\..+)?", p, re.I)
                for p in parts), f"unsafe evidence path {name!r}")
    return PurePosixPath(*parts)


def object_pairs(pairs):
    obj = {}
    for key, value in pairs:
        require(key not in obj, f"duplicate JSON key {key!r}")
        obj[key] = value
    return obj


def json_file(path):
    return json.loads(path.read_text(encoding="utf-8-sig"), object_pairs_hook=object_pairs,
                      parse_constant=lambda s: require(False, f"invalid JSON constant {s}"))


def sha(path):
    with path.open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest() if hasattr(hashlib, "file_digest") else hashlib.sha256(f.read()).hexdigest()


def validate_tree(folder):
    require(not folder.is_symlink() and not getattr(folder, "is_junction", lambda: False)(), "linked bundle root")
    files = {}
    seen = set()
    total = 0
    for p in folder.rglob("*"):
        require(not p.is_symlink() and not getattr(p, "is_junction", lambda: False)(),
                f"linked evidence forbidden: {p}")
        name = p.relative_to(folder).as_posix()
        safe_name(name)
        require(name.casefold() not in seen, f"path alias: {name}")
        seen.add(name.casefold())
        if p.is_dir():
            continue
        require(p.is_file(), f"not a regular file: {name}")
        size = p.stat().st_size
        total += size
        require(size <= MAX_MEMBER and total <= MAX_TOTAL and len(files) < 5000, "bundle limit exceeded")
        files[name] = p
    require("SHA256SUMS.json" in files, "bundle has no inventory")
    inventory = json_file(files.pop("SHA256SUMS.json"))
    require(isinstance(inventory, dict) and set(inventory) == set(files), "inventory/file set mismatch")
    for rel, path in files.items():
        want = inventory[rel]
        require(isinstance(want, str) and SHA_RE.fullmatch(want) and sha(path) == want,
                f"checksum mismatch: {rel}")
    return files


def intake(source, base):
    if source.is_dir():
        require(not source.is_symlink(), "symlinked bundle root")
        return source, validate_tree(source), None
    digest = sha(source)
    sidecar = source.with_name(source.name + ".sha256")
    if sidecar.exists():
        require(sidecar.read_text(encoding="utf-8-sig").split()[0] == digest, "ZIP SHA-256 mismatch")
    with zipfile.ZipFile(source) as z:
        entries = z.infolist()
        require(len(entries) <= 5000 and sum(p.file_size for p in entries) <= MAX_TOTAL, "ZIP limit exceeded")
        seen = set()
        files = []
        for entry in entries:
            name = entry.filename.rstrip("/") if entry.is_dir() else entry.filename
            rel = safe_name(name)
            require(name.casefold() not in seen, f"duplicate ZIP member {name}")
            filetype = stat.S_IFMT(entry.external_attr >> 16)
            require(filetype in (0, stat.S_IFREG, stat.S_IFDIR) and entry.file_size <= MAX_MEMBER,
                    f"non-regular or oversized ZIP member {name}")
            require(filetype != stat.S_IFDIR or entry.is_dir(), f"invalid directory {name}")
            seen.add(name.casefold())
            if not entry.is_dir():
                files.append((entry, rel))
        folder = base / digest[:20]
        if folder.exists():
            existing = validate_tree(folder)
            require(set(existing) | {"SHA256SUMS.json"} == {r.as_posix() for _, r in files}, "existing extracted bundle differs")
            for entry, rel in files:
                require((folder / rel).read_bytes() == z.read(entry), f"extraction differs: {rel}")
            return folder, existing, digest
        folder.mkdir(parents=True)
        try:
            for entry, rel in files:
                out = folder / rel
                out.parent.mkdir(parents=True, exist_ok=True)
                with z.open(entry) as src, out.open("xb") as dst:
                    shutil.copyfileobj(src, dst)
            return folder, validate_tree(folder), digest
        except (OSError, ValueError, zipfile.BadZipFile):
            shutil.rmtree(folder)
            raise


def iso(s):
    d = datetime.fromisoformat(s.replace("Z", "+00:00"))
    require(d.tzinfo is not None, "timestamp lacks timezone")
    return d.astimezone(timezone.utc)


def plan_poses(path):
    lines = [s.split("#")[0].strip() for s in path.read_text(encoding="utf-8").splitlines()]
    lines = [s for s in lines if s]
    map_id, hour, warm = lines[0].split()
    nodes = [s.split() for s in lines[1:]]
    indexes = [int(n[0]) for n in nodes]
    require(indexes[0] == 0 and indexes[-1] == 599 and indexes == sorted(set(indexes)), "not the 600-present scheme")
    expect = []
    for i, (lo, hi) in enumerate(zip(indexes, indexes[1:])):
        a, b = (tuple(map(float, n[1:])) for n in nodes[i:i + 2])
        for t in range(lo, hi):
            f = (t - lo) / (hi - lo)
            expect.append(tuple(x * (1 - f) + y * f for x, y in zip(a, b)))
    expect.append(tuple(map(float, nodes[-1][1:])))
    require(all(len(p) == 5 and all(map(math.isfinite, p)) for p in expect), "bad pose in plan")
    return (map_id, hour, warm), expect


def target_identity(adapter, gpu, backend="Vulkan"):
    # AdapterInfo.vendor/device is the PCI vendor/device ID. NVIDIA's SMI
    # field packs device in the high word and vendor in the low word.
    pci = int(gpu["device_id"], 16)
    require((pci & 0xffff) == 0x10de, "inventory GPU is not NVIDIA")
    return (f'name: "{TARGET_ADAPTER}"' in adapter and "device_type: DiscreteGpu" in adapter
            and "vendor: 4318," in adapter and f"device: {pci >> 16}," in adapter
            and f"backend: {backend} " in adapter
            and (backend == "Dx12" or bool(re.search(rf'(?<![\d.]){re.escape(gpu["driver"])}(?![\d.])', adapter))))


def match_target(meta, gpu, dx12=False):
    require(meta.get("platform") == "windows" and meta.get("architecture") == "x86_64", "not a Windows x64 capture")
    require(target_identity(meta["adapter"], gpu, "Dx12" if dx12 else "Vulkan"), "adapter/PCI/driver differs from session GPU")
    if not dx12:
        require(requested_target(meta), "Vulkan did not identify the RTX 3080 10 GB heap")


def validate_session(files, expected_commit, expected_build=None):
    session = json_file(files["session.json"])
    require(session.get("schema") == 1, "not a v1 performance session")
    require(session.get("prerequisites_ok"), f"prerequisites not met: {session.get('runner_error', 'no inventory')}")
    require(session.get("platform") == "windows" and session.get("architecture") == "x86_64"
            and session.get("display") == "2560x1440" and bool(session.get("machine")), "not a named 1440p Windows x64 session")
    gpu = session["gpu"]
    require(gpu["name"] == TARGET_ADAPTER and 9728 <= int(gpu["memory_mib"]) <= 10240
            and re.fullmatch(r"\d+\.\d+(?:\.\d+)?", gpu["driver"]), "not a 10 GB RTX 3080 driver inventory")
    require(any([item.strip() for item in row] == [str(gpu[k]) for k in
                 ("index", "name", "uuid", "pci_bus", "device_id", "driver", "memory_mib")]
                for row in csv.reader(io.StringIO(files["prerequisites/nvidia-gpu.csv"].read_text(encoding="utf-8")))),
            "GPU inventory does not match raw nvidia-smi")
    bt = session["build"]
    require(bt == json_file(files["prerequisites/build.json"]) and bt["schema"] == 1
            and re.fullmatch(r"[0-9a-f]{40}", bt["source_commit"]), "unbound build receipt")
    require(bt["source_commit"] == expected_commit, "kit source commit differs from the expected PR revision")
    require(expected_build is None or bt == expected_build, "kit manifest differs from trusted build.json")
    for name, key in [("Perfect-World-Explorer.exe", "viewer"), ("lighter-sdk-tests.exe", "smoke"), ("run-perf-session.ps1", "runner")]:
        receipt = session[key]
        require(SHA_RE.fullmatch(receipt["sha256"]) and receipt["sha256"] == bt["files"][name]
                and receipt["path"] and receipt["bytes"] > 0, f"{key} not bound to build")
    c = session["client"]["elementclient.exe"]
    require(c["sha256"] == PINS["element"] and c["bytes"] == 6000640 and bool(session["client_root"]), "not the pinned retail 1.4.2 executable")
    payload = files["travel.plan"]
    require(sha(payload) == session["plan"]["sha256"] == bt["files"]["a07-travel.plan"], "travel plan hash mismatch")
    require(payload.read_text(encoding="utf-8") == Path(__file__).with_name("a07-travel.plan").read_text(encoding="utf-8"),
            "not the checked-in a07 path")
    recipe, expect = plan_poses(payload)
    require(recipe == tuple(session["plan"][k] for k in ("map", "hour", "warmup")) == ("a07", "12", "60"), "initial path/settings mismatch")
    runs = {}
    for run in session["runs"]:
        base = f"{run['kind']}/{run['id']}"
        safe_name(base)
        require(run["id"] not in runs and run["dir"] == base
                and run == json_file(files[f"{base}/run.json"]), f"unbound run entry: {base}")
        require(run["executable_sha256"] == session["smoke" if run["kind"] == "smoke" else "viewer"]["sha256"],
                f"executable mismatch: {base}")
        runs[run["id"]] = run
    return session, runs, expect


def admitted(files, session, run):
    require(run.get("result") == "completed" and run.get("exit_code") == 0 and run.get("timeout") is None,
            f"{run.get('result', 'missing execution')}; exit={run.get('exit_hex')}; timeout={run.get('timeout')}; {run.get('reason', '')}".strip())
    require(iso(session["started_utc"]) <= iso(run["started_utc"]) < iso(run["ended_utc"]) <= iso(session["finished_utc"]), "run timestamp outside session")
    stderr = files[f"{run['dir']}/process.stderr.log"].read_text(encoding="utf-8", errors="replace")
    require(not PROBLEM.search(stderr), "device loss, crash or panic in process log")
    env = run["environment"]
    provider = run["requested_provider"]
    require(env.get("PW_RAY_RECONSTRUCTION") == ("off" if provider == "portable" else provider)
            and env.get("WGPU_BACKEND") == run["backend"] and not env.get("PW_RAY_RECONSTRUCTION_FALLBACK"),
            "request environment differs from run; explicit fallback not admitted")
    if provider != "portable":
        require(session["sdk_ready"].get(provider) and bool(session["sdk"]["fsr" if provider == "fsr" else "rr_feature"]), "requested SDK not pinned")
        pin = "fsr" if provider == "fsr" else "rr_feature"
        require(session["sdk"][pin]["sha256"] == PINS[pin], "SDK digest mismatch")
        if provider == "rr":
            require(SHA_RE.fullmatch(session["sdk"]["rr_bridge"]["sha256"]), "missing RR bridge hash")
            lib = session["sdk"].get("rr_loader_lib")
            require(not lib or lib["sha256"] == PINS["rr_loader_lib"], "RR loader library not pinned")
    return stderr


def first_present(files, session, run, log, want):
    start, ready, end = (iso(run[k]) for k in ("started_utc", "first_present_utc", "ended_utc"))
    ms = float(run["first_present_ms"])
    require(start < ready <= end and abs(ms - (ready - start).total_seconds() * 1000) < 2,
            "first-present timing not within process lifetime")
    require(files[f"{run['dir']}/present.ready"].read_bytes() == b"ready", "missing first-present marker")
    m = re.search(r"(?m)^Lighter perf first present: 2560x1440 mode=(Off|Software|Hardware) provider=(.+?) adapter=(AdapterInfo .*?)\r?$", log)
    require(m and m[1].lower() == run["mode"] and (want is None or m[2] == want), "first frame did not match route or provider")
    require(target_identity(m[3], session["gpu"], "Dx12" if run["backend"] == "dx12" else "Vulkan"), "first frame adapter mismatch")
    require(run["argv"] == [session["client_root"], "--world", session["plan"]["map"]], "client or map command differs")
    return ms


def provenance(meta, session, run):
    plan = session["plan"]
    require(meta["plan_sha256"] == plan["sha256"] and meta["map"] == plan["map"]
            and meta["display"] == "2560x1440" and meta["world"] == "2560x1440", "path, world or display differs")
    required_view = ("resolution_scale: 100.0", "upscaling: Off", "frame_generation: Off", "latency: Off",
                     "fullscreen: false", "weather_override: Clear", "time_pass_enabled: false", "all_to_vram: false")
    require(meta["view"].startswith("ViewSettings {") and all(" " + part + "," in meta["view"] for part in required_view)
            and sha_text(meta["view"]) == meta["view_sha256"], "graphics settings differ or digest mismatch")
    require(run["environment"].get("PW_PERF_CAPTURE") == "1", "not produced by the performance viewer")
    match_target(meta, session["gpu"], run["backend"] == "dx12")


def sha_text(s):
    return hashlib.sha256(s.encode()).hexdigest()


def admission_flags(run, candidate):
    env = run["environment"]
    require({key for key in GAME_FLAGS if env.get(key) == "1"} == (GAME_FLAGS if candidate else set()), "candidate flags differ")
    if candidate:
        require(all(env.get(f"PW_RAY_EFFECT_{k}") == "near" for k in ("REFLECTION", "SUN", "BOUNCE")), "effect tier differs")
    require(not env.get("PW_RAY_RECONSTRUCTION_SKIP") and "PW_LIGHTER_TRAVEL_CAPTURE_DIR" not in env,
            "bisection or image readback is not a travel companion")


def read_travel(files, session, run, route, policy, poses):
    mode, provider, candidate = ROUTES[route]
    require((run["kind"], run["policy"], run["requested_provider"], run["hybrid"], run["candidate"]) ==
            ("travel", policy, provider, mode == "Hybrid", candidate), "incorrect named route")
    require(run["backend"] == ("dx12" if route == "native-dx12" else "vulkan"), "incorrect backend request")
    require(run["environment"].get("PW_LIGHTER_TRAVEL_SYNC") == str(int(policy == "synchronized")), "capture policy differs")
    admission_flags(run, candidate)
    log = admitted(files, session, run)
    first_present(files, session, run, log, LABEL[provider])
    path = files[f"{run['dir']}/frames.csv"]
    meta, raw = travel.read_csv(path)
    record = travel.validate_rows(path, meta, raw, policy)
    provenance(meta, session, run)
    require(record[1] == mode and meta["ray_reconstruction"] == LABEL[provider], "actual renderer/provider differs")
    require(meta["hour"] == session["plan"]["hour"] and meta["warmup"] == session["plan"]["warmup"]
            and len(poses) == len(raw), "frame count, hour or warm-up not matched")
    require(all(math.isclose(x, y, rel_tol=0, abs_tol=6e-5) for a, b in zip(poses, record[2]) for x, y in zip(a, b)), "recorded camera differs from recipe")
    scale = int(session["ray_percent"])
    require(35 <= scale <= 50, "wrong ray scale policy")
    expected_trace = "0x0" if mode == "Off" else f"{2560 * scale // 100}x{1440 * scale // 100}"
    require(meta["trace"] == expected_trace, f"trace extent differs: expected {expected_trace}")
    required_ray = ("samples: 1", "bounces: 1", "denoiser: true", "progressive: false", "lighting: true",
                    "shadows: true", "reflections: true", "soft_sun: true", "single_path: true", "dynamic_resolution: false",
                    "fsr1_easu: false", "physical_bsdf: false", f"resolution: {scale}")
    require(all(" " + part + "," in meta["ray"] for part in required_ray) and meta["ray"].endswith("reference: false }"),
            "ray settings differ from fixed one-sample/one-bounce recipe")
    if route != "native-dx12":
        heap = [row[4] for row in record[3] if row[4] is not None]
        peak = int(meta.get("peak_sampled_device_local_heap_estimate_bytes", "0"))
        require(heap and min(heap) > 0 and peak >= max(heap) and len(heap) >= (len(raw) if policy == "synchronized" else len(raw)//30)
                and meta.get("heap_at_or_above_7_gb") == str(peak >= 7_000_000_000).lower(), "missing/inconsistent Vulkan heap evidence")
    if mode != "Off":
        require(all(row.get("actual_ray_provider") == LABEL[provider] for row in raw), "per-frame actual provider missing or fallback")
        require(all(row.get("game_profile_active") == str(int(candidate)) for row in raw), "candidate activity differs from request")
        require(all(str(row.get(k, '')).isdecimal() for row in raw for k in
                    ("primary_solid_queries_traced", "primary_solid_queries_skipped")), "missing primary slots")
        wrong = "primary_solid_queries_traced" if mode == "Hybrid" else "primary_solid_queries_skipped"
        require(all(int(row[wrong]) == 0 for row in raw), "primary query slots disagree with the executed route")
        if mode in ("Hybrid", "Hardware"):
            require(all(str(row.get(k, '')).isdecimal() for row in raw for k in
                        ("rt_unique_blas_triangles", "rt_tlas_instances")), "missing active AS counts")
    stats = travel.summarize(record)
    if mode != "Off":
        stats["rt_counts_max"] = [max((int(row[key]) for row in raw if row.get(key)), default=None)
                                  for key in ("primary_solid_queries_traced", "primary_solid_queries_skipped",
                                              "rt_unique_blas_triangles", "rt_tlas_instances")]
    if policy == "synchronized":
        stage_ids = [8] if mode == "Off" else [3, 6] + ([1] if mode in ("Hardware", "Hybrid") else []) + ([2] if mode == "Hybrid" else []) + ([4, 5] if provider == "portable" else [7])
        summed = []
        for row in record[3]:
            require(all(row[7][j] is not None and (j not in (2, 3, 7, 8) or row[7][j] > 0.) for j in stage_ids), "missing/non-positive current-frame GPU stage")
            summed.append(sum(row[7][j] for j in stage_ids))
        stats["gpu_scope"] = travel.durations(summed)
    return record, stats


def read_smoke(files, session, run, provider, mode, scale):
    require((run["kind"], run["requested_provider"], run["mode"], run["smoke_percent"], run["backend"]) ==
            ("smoke", provider, mode, scale, "vulkan"), "SDK smoke route/extent mismatch")
    env = run["environment"]
    require(env.get("PW_TEST_RAY_RESOLUTION") == str(scale) and
            env.get("PW_TEST_RAY_QUERY") == ("1" if mode == "hardware" else None), "wrong SDK smoke environment")
    require(run["argv"] == ["modules::lighter::reconstruction::tests::lighter_temporal_vendor_dispatch",
                            "--exact", "--ignored", "--nocapture", "--test-threads=1"], "not the strict vendor dispatch test")
    log = admitted(files, session, run)
    out = files[f"{run['dir']}/process.stdout.log"].read_text(encoding="utf-8", errors="replace")
    require(re.search(r"test result: ok\. 1 passed; 0 failed; 0 ignored;", out), "vendor test did not execute successfully")
    ad = re.search(r"(?m)^Vendor reconstruction adapter: (AdapterInfo .*?)\r?$", log)
    require(ad and target_identity(ad[1], session["gpu"]), "SDK smoke not on the session GPU/driver")
    for side in (512, 640):
        n = re.findall(rf"(?m)^{re.escape(LABEL[provider])} vendor smoke {side}²: (\d+)/(\d+) opaque pixels have positive SDK radiance\r?$", log)
        require(len(n) == 1 and side * side >= int(n[0][1]) >= int(n[0][0]) > 100, f"no positive finite raw {provider} {side}² output")
    return "V"


def read_startup(files, session, run):
    require(run["kind"] == "startup" and run["mode"] == "hardware" and not run["skip"]
            and run["requested_provider"] == session["startup_provider"] and run["backend"] == "vulkan"
            and run["environment"].get("PW_RAY_PACED_STARTUP") == "1", "startup settings differ")
    log = admitted(files, session, run)
    ms = first_present(files, session, run, log, LABEL[run["requested_provider"]])
    start, stop = iso(run["started_utc"]), iso(run["ended_utc"])
    for name in ("Application", "System"):
        event = run["events"][name]
        require(event["query"] == "ok" and iso(event["from_utc"]) == start and iso(event["through_utc"]) == stop,
                f"missing exact-window {name} event query")
        raw = files[f"{run['dir']}/events-{name}.txt"].read_text(encoding="utf-8", errors="replace")
        require(f"query=ok; count={event['count']}" in raw, f"{name} event text disagrees with receipt")
        require(event["count"] == 0, f"{name} has relevant events; review before startup sign-off")
    require(run.get("sampler_exited_early") is False and not run.get("sampler_error"), "dedicated-memory sampler failed")
    with files[f"{run['dir']}/nvidia-memory.csv"].open(encoding="utf-8", newline="") as f:
        data = list(csv.DictReader(f))
    gpu = session["gpu"]
    samples = []
    offset = timezone(timedelta(minutes=int(run["local_utc_offset_minutes"])))
    for row in data:
        require(row["gpu_uuid"].strip() == gpu["uuid"] and row["gpu_index"].strip() == gpu["index"], "memory sampler GPU changed")
        n = int(row["used_MiB"])
        require(0 <= n <= int(row["total_MiB"]) == int(gpu["memory_mib"]), "invalid dedicated memory sample")
        time = row["timestamp_local"].strip()
        stamp = datetime.strptime(time, "%Y/%m/%d %H:%M:%S" + (".%f" if "." in time else ""))
        samples.append((stamp.replace(tzinfo=offset).astimezone(timezone.utc), n))
    require(len(samples) >= 3 and samples[0][0] < start < stop < samples[-1][0]
            and all(0 < (b[0] - a[0]).total_seconds() <= 2.5 for a, b in zip(samples, samples[1:])),
            "1-second memory stream does not bracket the launch")
    submits = [float(t) for t in re.findall(r"(?m)^Lighter startup GPU submit: ([0-9.]+) ms\r?$", log)]
    return {"samples": samples, "first_present_ms": ms, "submit": submits,
            "loaded_cache": bool(re.search(r"Lighter Vulkan pipeline cache: loaded [0-9]+ bytes", log))}


def read_memory(files, session, run, candidate):
    require((run["kind"], run["policy"], run["mode"], run["hybrid"], run["candidate"], run["requested_provider"]) ==
            ("memory", "memory_travel", "hardware", True, True, "portable"), "wrong memory profile")
    admission_flags(run, True)
    log = admitted(files, session, run)
    first_present(files, session, run, log, "portable")
    path = files[f"{run['dir']}/heap.csv"]
    meta, rows = travel.read_csv(path)
    provenance(meta, session, run)
    require(meta.get("schema") == "1" and meta.get("capture_policy") == "memory_travel"
            and set(rows[0]) == {"elapsed_s", "frame", "chunk_x", "chunk_z", "commits",
                                 "device_local_heap_estimate_bytes", "game_profile_active"}
            and len(rows) == int(meta["count"]) >= 2, "malformed separate memory receipt")
    for key in ("plan_sha256", "display", "world", "trace", "ray", "hybrid", "ray_reconstruction", "present_mode", "adapter", "view_sha256"):
        require(meta[key] == candidate[0][key], f"memory vs candidate {key} mismatch")
    t = [float(r["elapsed_s"]) for r in rows]
    peak = max(int(r["device_local_heap_estimate_bytes"]) for r in rows)
    goal = int(session["memory_goal_s"])
    require(run["environment"].get("PW_PERF_MEMORY_SECONDS") == str(goal)
            and 600 <= goal <= 3600 and t[0] == 0 and t[-1] >= goal
            and t[-1] <= (iso(run["ended_utc"]) - iso(run["first_present_utc"])).total_seconds() + .01
            and all(map(math.isfinite, t)) and all(0 < b - a <= 2.5 for a, b in zip(t, t[1:])), "incomplete time coverage (>=600 s at ~1 Hz required)")
    require(all(int(r["device_local_heap_estimate_bytes"]) > 0 and r["game_profile_active"] == "1" for r in rows), "invalid/absent process heap or candidate activity")
    frame = [int(r["frame"]) for r in rows]
    commits = [int(r["commits"]) for r in rows]
    chunks = {(int(r["chunk_x"]), int(r["chunk_z"])) for r in rows}
    require(frame[0] == 0 and all(b > a for a, b in zip(frame, frame[1:]))
            and commits == sorted(commits) and commits[0] >= 0 and commits[-1] > 0 and len(chunks) > 1,
            "memory route did not show streamed progress")
    return peak, t[-1], len(rows), commits[-1]


def md(value):
    # Logs/manifests are user data, not Markdown or HTML to execute/render.
    text = str(value).replace("\r", " ").replace("\n", " ").replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
    return re.sub(r"([\\|`*_[\]])", r"\\\1", text)


def fmt(number, precision=2):
    return "—" if number is None else f"{number:.{precision}f}"


def analyze(files, report_path, zip_digest, expected_commit, expected_build=None):
    """Return report text and the strict gate. Broken routes do not erase good ones."""
    session, runs, poses = validate_session(files, expected_commit, expected_build)
    outcomes = {}
    rows = {}
    issues = {}

    def link(rel, title):
        if rel not in files:
            return "—"
        path = os.path.relpath(files[rel], report_path.parent).replace(os.sep, "/")
        return f"[{title}]({quote(path, safe='/.-_')})"

    def invoke(id_, fn, *args):
        try:
            return fn(files, session, runs[id_], *args)
        except (KeyError, IndexError, ValueError, TypeError, OSError) as error:
            issues[id_] = str(error) if id_ in runs else "not run"
            return None

    def run_links(id_, raw):
        if id_ not in runs:
            return "—"
        d = runs[id_]["dir"]
        return ", ".join(x for name, caption in [("run.json", "receipt"), (raw, "raw"), ("process.stderr.log", "stderr")]
                         if (x := link(f"{d}/{name}", caption)) != "—")

    def matched(a, b, *, candidate=False):
        try:
            travel.validate_pair(a, b, sampled_bc_ab=candidate)
            return True, ""
        except ValueError as error:
            return False, str(error)

    verified_smoke = {}
    for provider in ("rr", "fsr"):
        for mode in ("hardware", "software"):
            for scale in (50, 100):
                id_ = f"{provider}-{mode}-{scale}"
                verified_smoke[(provider, mode, scale)] = invoke(id_, read_smoke, provider, mode, scale)

    lines = ["# Lighter Windows performance results", "",
             "Measured route receipts; P = Windows RTX 3080 portable/Native; V = actual verified SDK dispatch. "
             "Neither denotes a timing/quality pass by itself.", "",
             f"- Host: {md(session['machine'])} · {md(session['os'])} · RTX 3080 {session['gpu']['memory_mib']} MiB · NVIDIA {session['gpu']['driver']}",
             f"- Exact kit source: `{expected_commit}`; viewer `{session['viewer']['sha256']}`; test binary `{session['smoke']['sha256']}`",
             f"- {link('session.json','Session inventory')} · {link('prerequisites/build.json','kit receipt')} · {link('travel.plan','camera recipe')} · {link('SUMMARY.txt','operator summary')}",
             f"- Output 2560×1440, {session['ray_percent']}% ray scale; ZIP SHA-256 `{zip_digest or 'folder input'}`.",
             "- Normal-present intervals are CPU-observed present cadence, not scanout. Synchronized rows are a different run. "
             "GPU scope is the sum of measured applicable AS/raster/trace/portable-post/compose/SDK spans; Native uses the bracketing world span.", "",
             "## Startup", "",
             "| Start | Outcome | First present (ms) | Last stage | Exit | Dedicated GPU memory pre / peak / post (MiB) | Largest paced submit (ms) / cache hit | Receipts |",
             "|---|---|---:|---|---|---:|---:|---|"]
    start_ids = ["hardware-1", "hardware-2"]
    startup = [invoke(id_, read_startup) for id_ in start_ids]
    interval = None
    if all(startup):
        interval = (iso(runs["hardware-2"]["started_utc"]) - iso(runs["hardware-1"]["ended_utc"])).total_seconds()
    for id_ in start_ids + sorted(i for i in runs if i.startswith("bisect-")):
        r = runs.get(id_, {})
        d = r.get("dir", "startup/" + id_)
        raw = ", ".join([run_links(id_, "stages.txt"), link(f"{d}/events-Application.txt", "Application"),
                         link(f"{d}/events-System.txt", "System"), link(f"{d}/nvidia-memory.csv", "dedicated memory")])
        outcome = ("clean start" if id_ in start_ids and id_ not in issues else issues.get(id_, r.get("result", "not run")))
        verified = startup[start_ids.index(id_)] if id_ in start_ids else None
        dedicated = gpu_details = "—"
        if verified:
            samples = verified["samples"]
            dedicated = f"{samples[0][1]} / {max(n for _, n in samples)} / {samples[-1][1]}"
            gpu_details = f"{fmt(max(verified['submit'], default=None),3)} / {'yes' if verified['loaded_cache'] else 'no'}"
        lines.append(f"| {id_} | {md(outcome)} | {fmt(r.get('first_present_ms'))} | {md(r.get('last_stage') or '—')} | {md(r.get('exit_hex') or '—')} | {dedicated} | {gpu_details} | {raw} |")
    startup_ok = interval is not None and interval >= 60
    lines.extend(["", f"Two-start gate: **{'PASS' if startup_ok else 'OPEN/FAIL'}**. Exit-to-next-launch gap: {fmt(interval)} s (≥60 s). "
                  "Event matches require review; the table does not diagnose a TDR or fault module. Ten consecutive clean starts are a separate gate.",
                  "", "## Matched 1440p travel", "",
                  "| Route | Evidence / actual | N | Median ms | p95 ms | Slowest 1% mean ms / fps | GPU p50 / p95 / max ms | Peak driver heap (GB) | Receipts / reason |",
                  "|---|---|---:|---:|---:|---:|---:|---:|---|"])
    for route in ROUTES:
        for policy in POLICIES:
            id_ = f"{route}-{policy}"
            outcome = invoke(id_, read_travel, route, policy, poses)
            if outcome:
                outcomes[id_] = outcome
        a, b = (outcomes.get(f"{route}-{p}") for p in POLICIES)
        trouble = [f"{p}: {issues[f'{route}-{p}']}" for p in POLICIES if f"{route}-{p}" in issues]
        pair_ok = False
        if a and b:
            pair_ok, reason = matched(a[0], b[0])
            if not pair_ok:
                trouble.append(reason)
        # All recipe/graphics fields must match the Vulkan Native control. All
        # ray rows must also match Hardware's effective ray settings, except
        # the admitted BC change in the explicit candidate. Never compare
        # DX12 seconds/memory to Vulkan as though they used the same API.
        if pair_ok and route not in ("native", "native-dx12"):
            for control in ("native", "hardware"):
                c = outcomes.get(f"{control}-normal_present")
                if c and route != control:
                    ok, reason = matched(c[0], a[0], candidate=route == "candidate")
                    if not ok:
                        pair_ok = False
                        trouble.append(f"vs {control}: {reason}")
        mode, provider, _ = ROUTES[route]
        actual = (a or b)[1]["provider"] if a or b else "unverified"
        if pair_ok and provider != "portable" and not all(verified_smoke.get((provider, mode.lower(), x)) == "V" for x in (50, 100)):
            trouble.append("SDK dispatch observed in travel; strict 50/100 raw SDK smoke incomplete (no V label)")
            label = "unverified"
        else:
            label = ("P" if provider == "portable" else "V") if pair_ok else "unverified"
        normal = a[1]["normal_present_calls"] if a else {}
        gpu = b[1]["gpu_scope"] if b else {}
        heap = max((item[1]["peak_sampled_device_local_heap_estimate_bytes"] or 0 for item in (a,b) if item), default=0)
        refs = "; ".join(run_links(f"{route}-{p}", "frames.csv") for p in POLICIES)
        if trouble:
            refs += "; " + md("; ".join(trouble))
        lines.append(f"| {route} | {label} / {md(actual)} | {normal.get('n','—')} | {fmt(normal.get('p50_ms'))} | "
                     f"{fmt(normal.get('p95_ms'))} | {fmt(normal.get('slowest_one_percent_ms'))} / {fmt(normal.get('slowest_one_percent_fps'))} | "
                     f"{fmt(gpu.get('p50_ms'))} / {fmt(gpu.get('p95_ms'))} / {fmt(gpu.get('max_ms'))} | {fmt(heap/1e9 if heap else None,3)} | {refs} |")
        if pair_ok:
            rows[route] = (a, b)

    lines.extend(["", "The separate `native-dx12` row checks Native on its usual backend; it is not the Vulkan control. "
                  "No provider timing is substituted for a failed SDK run.", "", "### Synchronized stage detail", "",
                  "Cells are **median / p95 / maximum ms**. A missing stage is shown as —, not zero.", "",
                  "| Route | AS | Raster primary | Trace | Reproject | Filter | Ray composite | SDK | Native world | Queue drain wall |",
                  "|---|---|---|---|---|---|---|---|---|---|"])
    for route in ROUTES:
        item = outcomes.get(f"{route}-synchronized")
        if not item:
            continue
        stages = item[1]["synchronized_stages"]
        cells = []
        for key in (*travel.STAGES[1:], travel.STAGES[0]):
            s = stages[key]
            cells.append(" / ".join(fmt(s[k],3) for k in ("p50_ms","p95_ms","max_ms")) if s else "—")
        lines.append("| " + " | ".join([route] + cells) + " |")

    lines.extend(["", "### Executed primary and acceleration counts", "",
                  "Maxima from normal-present rows; primary counters are first-solid slots, not shading rays or intersections.", "",
                  "| Route | First-solid traced | First-solid skipped | Unique BLAS triangles | TLAS instances |",
                  "|---|---:|---:|---:|---:|"])
    for route in ROUTES:
        item = outcomes.get(f"{route}-normal_present")
        if item and "rt_counts_max" in item[1]:
            cells = [f"{n:,}" if n is not None else "—" for n in item[1]["rt_counts_max"]]
            lines.append("| " + " | ".join([route] + cells) + " |")

    lines.extend(["", "## Strict raw SDK smoke", "", "| Request | Route / ray % | Outcome | Receipts |", "|---|---|---|---|"])
    for (provider, mode, scale), value in verified_smoke.items():
        id_ = f"{provider}-{mode}-{scale}"
        lines.append(f"| {provider} | {mode} / {scale}% | {value or md(issues[id_])} | {run_links(id_, 'process.stdout.log')} |")

    candidate = rows.get("candidate")
    mem = invoke("candidate-memory", read_memory, candidate[0][0]) if candidate else None
    reach = [False] * 4
    timing_detail = "Candidate is missing a valid matched pair."
    crossing_detail = "No matched candidate travel."
    vs = []
    if candidate:
        n, g = candidate
        st = n[1]["normal_present_calls"]
        cost = g[1]["gpu_scope"]
        reach[0] = st["p50_ms"] <= 16.6 and st["slowest_one_percent_ms"] <= 25.
        reach[1] = cost["max_ms"] <= 5.
        periods = [r[0] for r in n[0][3] if r[2] or r[3]]
        reach[2] = n[1]["stream_commits"] > 0 and bool(periods) and max(periods) <= 50.
        timing_detail = f"Median {st['p50_ms']:.3f} ms (≤16.6); slowest 1% mean {st['slowest_one_percent_ms']:.3f} ms (≤25); max ray scope {cost['max_ms']:.3f} ms (≤5)."
        crossing_detail = f"{n[1]['stream_commits']} commits; worst streaming/commit interval {fmt(max(periods, default=None),3)} ms (≤50)."
        for control in ("hardware", "hybrid"):
            a = rows.get(control)
            if not a:
                vs.append((control, False, "missing baseline"))
                continue
            compatible, reason = matched(a[0][0], n[0], candidate=True)
            if not compatible:
                vs.append((control, False, reason))
                continue
            baseline = a[0][1]["normal_present_calls"]
            p50 = st["p50_ms"] - baseline["p50_ms"]
            tail = st["slowest_one_percent_ms"] - baseline["slowest_one_percent_ms"]
            vs.append((control, p50 < 0 and tail < 0, f"Δmedian {p50:+.3f} ms, Δslowest-1% {tail:+.3f} ms"))
    reach[3] = bool(mem and mem[0] < 7_000_000_000)
    lines.extend(["", "## Candidate gates and next checks", "",
                  f"- Normal cadence: **{'PASS' if reach[0] else 'OPEN/FAIL'}**. {timing_detail}",
                  f"- Ray GPU scope: **{'PASS' if reach[1] else 'OPEN/FAIL'}**. Every synchronized ray scope must be ≤5 ms.",
                  f"- Streaming crossing: **{'PASS' if reach[2] else 'OPEN/FAIL'}**. {crossing_detail}",
                  f"- Memory: **{'PASS' if reach[3] else 'OPEN/FAIL'}**. " +
                  (f"{mem[0]:,} B peak VK_EXT_memory_budget estimate over {mem[1]:.3f} s ({mem[2]} samples, {mem[3]} commits)." if mem else
                   md(issues.get("candidate-memory", "Missing matched candidate or ≥600 s memory run."))) + " " + run_links("candidate-memory", "heap.csv")])
    for name, win, detail in vs:
        lines.append(f"- Candidate vs {name}: **{'both improved' if win else 'no matched win'}**; {md(detail)}.")
    lines.extend(["", "### Plan evidence", "", "| Item | Evidence present | What remains |", "|---|---|---|"])
    texts = {
        "A1": ("S ABI/preflight/bisection", "Use exact failing start logs before attributing a fault"),
        "A2": ("S paced residency/watchdog/cache; startup logs above", "Verify cache hit and <1 ms uploads/builds on retail hardware"),
        "A3": ("S two injected ownership cleanups", "Real driver recovery or VRAM reclamation is not simulated"),
        "A4": ("S shader/route controls; Hybrid travel slots if admitted", "Matched glass/cutout/water/effect color and depth on target"),
        "A5": ("S exact duplicate proxy and static compaction lane", "Class LOD, size culls/merges, matched reflection/shadow quality"),
        "A6": ("S transport specializations; per-draw candidate admission", "Separate effect counters, independent reflection resolution and far raster cascades"),
        "A7": ("S isolated reflection/shadow filter and generated noise", "Real raster motion/effect-plane integration, convergence and ghosting against old filter"),
        "A8": ("S fail-closed; V only where strict smoke passes above", "Missing/rejected SDK rows remain open"),
        "A9": ("S adaptive timestamp and Naga fixtures", "Matched adaptive/native-internal scale evaluation"),
        "A10": ("S ceiling and staging; sampled P heap above where admitted", "Whole-world/driver/long travel behavior beyond this recipe"),
        "B1–B4": ("Raw inventory, first-present, event and dedicated-memory files are linked", "Resolve untrustworthy/incomplete windows and errors above"),
        "B5–B6": ("Matched travel and SDK verdicts shown by row", "Supply failed/missing routes separately; never relabel fallback"),
        "B7/C": ("SHA-256 inventory verified; raw paths linked", "Retain the exact source and returned ZIP; disclose any rerun separately"),
        "D": ("Handoff documents operator procedure", "Review screen observations, content parity and ten clean starts before promotion"),
    }
    for key, (good, open_) in texts.items():
        lines.append(f"| {key} | {good} | {open_} |")
    strict = (startup_ok and all(reach) and len(vs) == 2 and all(x[1] for x in vs)
              and all(route in rows for route in ROUTES) and all(verified_smoke.values()))
    lines.extend(["", f"**Recorded performance gates: {'PASS' if strict else 'OPEN/FAIL'}.** "
                  "This report cannot promote defaults or substitute for visual parity and driver review.", ""])
    return "\n".join(lines), bool(strict)


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("bundle", type=Path, help="returned ZIP or extracted bundle directory")
    cli.add_argument("--expect-commit", required=True, help="exact 40-digit kit source SHA from the trusted PR build")
    cli.add_argument("--expect-build", type=Path, help="build.json from the trusted kit (binds executable and script hashes too)")
    cli.add_argument("--output", type=Path, default=ROOT / "Modules/Lighter/PERF-RESULTS.md")
    cli.add_argument("--evidence-dir", type=Path, default=ROOT / "cache/lighter-perf/returned")
    args = cli.parse_args()
    try:
        require(re.fullmatch(r"[0-9a-f]{40}", args.expect_commit), "expected commit must be the exact PR SHA")
        folder, files, digest = intake(args.bundle.absolute(), args.evidence_dir.absolute())
        require(not args.output.resolve().is_relative_to(folder.resolve()), "do not write reports into the raw evidence tree")
        expected_build = json_file(args.expect_build) if args.expect_build else None
        text, ok = analyze(files, args.output.absolute(), digest, args.expect_commit, expected_build)
    except (OSError, zipfile.BadZipFile, ValueError, KeyError, IndexError, TypeError, RuntimeError) as error:
        print(f"Rejected capture: {error}", file=sys.stderr)
        return 2
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(text, encoding="utf-8")
    print(f"{args.output}: {'recorded gates pass' if ok else 'one or more gates open/failed'}")
    print("See the Plan evidence table for item-by-item evidence and remaining checks.")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
