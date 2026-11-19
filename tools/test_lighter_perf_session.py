"""Parser fixtures only: temporary synthetic data, never target receipts."""
import csv
from datetime import timedelta
import importlib.util
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
import zipfile

SPEC = importlib.util.spec_from_file_location("perf", Path(__file__).with_name("windows") / "analyze-perf-session.py")
perf = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(perf)

COMMIT = "a" * 40
SHA = "0" * 64
GPU = dict(index="0", name=perf.TARGET_ADAPTER, uuid="GPU-test", pci_bus="00000000:01:00.0",
           device_id="0x220610DE", driver="580.88", memory_mib="10240")
ADAPTER = ('AdapterInfo { name: "NVIDIA GeForce RTX 3080", vendor: 4318, device: 8710, '
           'device_type: DiscreteGpu, driver: "NVIDIA", driver_info: "580.88", backend: Vulkan }')
VIEW = ('ViewSettings { resolution_scale: 100.0, upscaling: Off, frame_generation: Off, latency: Off, '
        'fullscreen: false, weather_override: Clear, time_pass_enabled: false, all_to_vram: false, extra: 0 }')
RAY = ('shadows: true, reflections: true, soft_sun: true, lighting: true, samples: 1, bounces: 1, '
       'progressive: false, denoiser: true, single_path: true, dynamic_resolution: false, fsr1_easu: false, '
       'physical_bsdf: false, resolution: 50, reference: false }')
START = "2026-11-18T09:00:04Z"
END = "2026-11-18T09:00:06Z"


class PerfSessionTests(unittest.TestCase):
    def setUp(self):
        temp = TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.tmp = Path(temp.name)
        self.bundle = self.tmp / "bundle"
        self.plan = (Path(perf.__file__).with_name("a07-travel.plan").read_text())
        plan = self.write("travel.plan", self.plan)
        _, self.poses = perf.plan_poses(plan)
        exe = {"bytes": 1, "sha256": SHA, "path": "synthetic"}
        self.build = {"schema": 1, "source_commit": COMMIT, "run_id": "test", "files": {
            "Perfect-World-Explorer.exe": SHA, "lighter-sdk-tests.exe": SHA,
            "run-perf-session.ps1": SHA, "a07-travel.plan": perf.sha(plan)}}
        self.manifest = dict(schema=1, prerequisites_ok=True, platform="windows", architecture="x86_64", os="synthetic",
                             display="2560x1440", machine="synthetic-parser-fixture", gpu=GPU, build=self.build,
                             viewer=exe, smoke=exe, runner=exe, client_root="E:\\pw142", startup_provider="portable",
                             ray_percent=50, memory_goal_s=600, started_utc="2026-11-18T09:00:00Z",
                             finished_utc="2026-11-18T14:00:00Z", sdk_ready={s: True for s in ("fsr", "rr")},
                             sdk={s:{"sha256":perf.PINS[s]} for s in ("fsr", "rr_feature")} | {"rr_bridge":exe},
                             client={"elementclient.exe":{"bytes":6000640,"sha256":perf.PINS["element"]}},
                             plan=dict(sha256=perf.sha(plan),map="a07",hour="12",warmup="60"), runs=[])
        self.write("prerequisites/build.json", self.build)
        self.write("prerequisites/nvidia-gpu.csv", ", ".join(GPU.values()) + "\n")
        self.write("SUMMARY.txt", "Synthetic parser test; not P or V")

    def write(self, name, text):
        path = self.bundle / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text if isinstance(text, str) else json.dumps(text), encoding="utf-8")
        return path

    def run_record(self, ident, kind, mode="Hardware", provider="portable", candidate=False, hybrid=False, dx=False, start=START, end=END):
        backend = "dx12" if dx else "vulkan"
        env = {"PW_RAY_RECONSTRUCTION":provider if provider != "portable" else "off", "WGPU_BACKEND":backend,
               "PW_PERF_CAPTURE":"1"}
        if candidate:
            env.update({k:"1" for k in perf.GAME_FLAGS})
            env.update({f"PW_RAY_EFFECT_{k}":"near" for k in ("REFLECTION","SUN","BOUNCE")})
        dir_ = f"{kind}/{ident}"
        receipt = dict(id=ident, dir=dir_, kind=kind, mode=mode.lower(), backend=backend, requested_provider=provider,
                       candidate=candidate, hybrid=hybrid, skip="", policy="", result="completed", exit_code=0,
                       exit_hex="0x00000000", timeout=None, executable_sha256=SHA, environment=env,
                       argv=[self.manifest["client_root"],"--world","a07"], started_utc=start, ended_utc=end,
                       first_present_utc=(perf.iso(start)+timedelta(seconds=1)).isoformat(), first_present_ms=1000., last_stage="Presented")
        template = ADAPTER.replace("Vulkan", "Dx12") if dx else ADAPTER
        self.write(dir_+"/present.ready", "ready")
        self.write(dir_+"/process.stderr.log", f"Lighter perf first present: 2560x1440 mode={mode} provider={perf.LABEL[provider]} adapter={template}\n")
        self.manifest["runs"].append(receipt)
        return receipt

    def finalize(self):
        for r in self.manifest["runs"]:
            self.write(r["dir"] + "/run.json", r)
        self.write("session.json", self.manifest)
        inv = {p.relative_to(self.bundle).as_posix():perf.sha(p) for p in self.bundle.rglob("*")
               if p.is_file() and p.name != "SHA256SUMS.json"}
        self.write("SHA256SUMS.json", inv)
        return perf.validate_tree(self.bundle)

    def meta(self, sync, mode="Hardware", provider="portable", hybrid=False, candidate=False, dx=False):
        return dict(schema="1",capture_policy="synchronized" if sync else "normal_present",count="600",
                    platform="windows",architecture="x86_64",adapter=ADAPTER.replace("Vulkan","Dx12") if dx else ADAPTER,
                    vulkan_largest_device_local_heap_bytes=str(10*2**30),plan_sha256=self.manifest["plan"]["sha256"],map="a07",
                    display="2560x1440",world="2560x1440",trace="0x0" if mode == "Off" else "1280x720",present_mode="Mailbox",
                    view=VIEW,view_sha256=perf.sha_text(VIEW),warmup="60",hour="12",hybrid=str(hybrid).lower(),
                    ray=f"Settings {{ mode: {mode}, hybrid_primary: {str(hybrid).lower()}, {RAY}",ray_reconstruction=perf.LABEL[provider],
                    screen_cache_mask="0",compact_terrain="false",sampled_bc=str(candidate).lower(),
                    peak_sampled_device_local_heap_estimate_bytes="5998000000",heap_at_or_above_7_gb="false")

    def write_csv(self, path, meta, frames):
        with self.write(path,"" ).open("w", newline="", encoding="utf-8") as out:
            out.write("".join(f"# {k}={v}\n" for k,v in meta.items()))
            writer = csv.DictWriter(out, fieldnames=list(frames[0]))
            writer.writeheader()
            writer.writerows(frames)

    def frames(self, route, *, override=None, missing_heap=False):
        want, provider, candidate = perf.ROUTES[route]
        mode = "Hardware" if want == "Hybrid" else want
        dx = route == "native-dx12"
        for policy in perf.POLICIES:
            sync = policy == "synchronized"
            r = self.run_record(f"{route}-{policy}","travel",mode,provider,candidate,want == "Hybrid",dx)
            r["policy"] = policy
            r["environment"]["PW_LIGHTER_TRAVEL_SYNC"] = str(int(sync))
            rows = []
            for i, pose in enumerate(self.poses):
                row = dict(frame=i, interval_ms=15 if candidate else 19, redraw_ms="6", acquire_ms=".04", present_call_ms=".06",
                           streaming=int(i==90), committed=int(i==91), device_local_heap_estimate_bytes="" if dx or missing_heap else 5998000000)
                row.update(zip(perf.travel.POSE, pose))
                if sync:
                    row.update({k:"" for k in perf.travel.STAGES})
                    row["queue_drain_wall_ms"] = "2"
                    if mode == "Off":
                        row["gpu_native_world_span_ms"] = ".5"
                    else:
                        for s in perf.travel.STAGES[1:-1]:
                            if s != "gpu_sdk_ms" or provider != "portable":
                                row[s] = "0.5"
                if mode != "Off":
                    row.update(actual_ray_provider=override or perf.LABEL[provider], game_profile_active=str(int(candidate)),
                               primary_solid_queries_traced="0" if want == "Hybrid" else "10",
                               primary_solid_queries_skipped="5" if want == "Hybrid" else "0",rt_unique_blas_triangles="100",
                               rt_tlas_instances="10")
                rows.append(row)
            self.write_csv(r["dir"]+"/frames.csv", self.meta(sync,mode,provider,want=="Hybrid",candidate,dx), rows)

    def sample_memory(self, *, span=600, peak=6_999_999_999, commits=True):
        r = self.run_record("candidate-memory","memory",candidate=True,hybrid=True,end="2026-11-18T09:12:00Z")
        r["policy"] = "memory_travel"
        r["environment"]["PW_PERF_MEMORY_SECONDS"] = "600"
        meta = self.meta(False,hybrid=True,candidate=True) | {"capture_policy":"memory_travel", "count":str(span+1)}
        rows = [dict(elapsed_s=i,frame=i*60,chunk_x=i%3,chunk_z=i%4,commits=int(commits)*i,device_local_heap_estimate_bytes=peak,
                     game_profile_active=1) for i in range(span+1)]
        self.write_csv(r["dir"]+"/heap.csv",meta,rows)

    def start(self, ident, *, second=False, event_error=False, clean_gap=True):
        offset = 62 if second and clean_gap else 60 if second else 0
        a,b = [(perf.iso(t)+timedelta(seconds=offset)).isoformat() for t in (START,END)]
        r = self.run_record(ident,"startup",start=a,end=b)
        r["environment"]["PW_RAY_PACED_STARTUP"] = "1"
        r.update(sampler_exited_early=False,local_utc_offset_minutes=120)
        r["events"] = {}
        for channel in ("System","Application"):
            status = "error" if event_error else "ok"
            r["events"][channel] = dict(query=status,count=0,from_utc=a,through_utc=b)
            self.write(f"{r['dir']}/events-{channel}.txt", f"query={status}; count=0\n")
        self.write(r["dir"]+"/nvidia-memory.csv", "timestamp_local,gpu_index,gpu_uuid,total_MiB,used_MiB\n" + "".join(
            (perf.iso(a)+timedelta(hours=2,seconds=i)).strftime("%Y/%m/%d %H:%M:%S.000") + f",0,GPU-test,10240,{(i+2)*100}\n"
            for i in range(-2,5)))

    def smoke(self, provider, mode, scale, *, black=False):
        r = self.run_record(f"{provider}-{mode.lower()}-{scale}","smoke",mode,provider)
        r["smoke_percent"] = scale
        r["environment"]["PW_TEST_RAY_RESOLUTION"] = str(scale)
        if mode == "Hardware":
            r["environment"]["PW_TEST_RAY_QUERY"] = "1"
        r["argv"] = ["modules::lighter::reconstruction::tests::lighter_temporal_vendor_dispatch","--exact","--ignored","--nocapture","--test-threads=1"]
        self.write(r["dir"]+"/process.stderr.log", f"Vendor reconstruction adapter: {ADAPTER}\n" + "".join(
            f"{perf.LABEL[provider]} vendor smoke {side}²: {0 if black else 101}/200 opaque pixels have positive SDK radiance\n" for side in (512,640)))
        self.write(r["dir"]+"/process.stdout.log", "test result: ok. 1 passed; 0 failed; 0 ignored;\n")
        return r

    def test_archive_inventory_and_existing_tree_round_trip(self):
        self.finalize()
        archive = self.tmp / "capture.zip"
        with zipfile.ZipFile(archive, "w") as z:
            for f in self.bundle.rglob("*"):
                if f.is_file():
                    z.write(f,f.relative_to(self.bundle).as_posix())
        folder, files, digest = perf.intake(archive,self.tmp/"expanded")
        self.assertEqual(files["travel.plan"].read_text(),self.plan)
        self.assertEqual(digest,perf.sha(archive))
        self.assertEqual(perf.intake(archive,self.tmp/"expanded")[0],folder)
        files["travel.plan"].write_text("tampered")
        with self.assertRaisesRegex(ValueError,"checksum mismatch"):
            perf.intake(archive,self.tmp/"expanded")

    def test_rejects_unsafe_archive_paths_and_duplicate_alias(self):
        for names in [("../outside",), ("evil\\file",), ("CON.txt",), ("x", "X"), ("C:/test",)]:
            with self.subTest(names=names):
                archive = self.tmp/"unsafe.zip"
                with zipfile.ZipFile(archive,"w") as z:
                    for name in names:
                        z.writestr(name,"do not extract")
                with self.assertRaisesRegex(ValueError,"unsafe evidence path|duplicate ZIP"):
                    perf.intake(archive,self.tmp/"expanded")
                self.assertFalse((self.tmp/"expanded").exists())

    def test_rejects_untrusted_build_or_driver(self):
        files = self.finalize()
        with self.assertRaisesRegex(ValueError,"expected PR revision"):
            perf.validate_session(files,"c"*40)
        with self.assertRaisesRegex(ValueError,"trusted build"):
            perf.validate_session(files,COMMIT,self.build | {"run_id":"other"})
        self.assertFalse(perf.target_identity(ADAPTER.replace("580.88","580.881"),GPU))
        self.assertFalse(perf.target_identity(ADAPTER.replace("device: 8710","device: 1"),GPU))

    def test_partial_failures_report_without_passing_candidate(self):
        self.frames("candidate", missing_heap=True)
        self.frames("hardware-rr", override="portable")
        self.start("hardware-1",event_error=True)
        self.smoke("rr","Hardware",50,black=True)
        report, ok = perf.analyze(self.finalize(),self.tmp/"out.md",None,COMMIT)
        self.assertFalse(ok)
        for text in ["inconsistent Vulkan heap evidence", "per-frame actual provider", "no positive finite raw rr",
                     "missing exact-window Application", "Missing matched candidate", "OPEN/FAIL"]:
            self.assertIn(text,report)
        self.assertNotIn("| hardware-rr | V /",report)

    def test_memory_must_cover_ten_minutes_and_progress(self):
        meta = self.meta(False,hybrid=True,candidate=True)
        for span, commits in [(599,True),(600,False)]:
            with self.subTest(span=span,commits=commits):
                self.manifest["runs"].clear()
                self.sample_memory(span=span,commits=commits)
                files = self.finalize()
                run = self.manifest["runs"][0]
                with self.assertRaisesRegex(ValueError,"incomplete time coverage|streamed progress"):
                    perf.read_memory(files,self.manifest,run,(meta,))

    def test_complete_fixture_computes_tables_and_strict_decimal_gate(self):
        self.start("hardware-1")
        self.start("hardware-2",second=True)
        for route in perf.ROUTES:
            self.frames(route)
        for provider in ("rr","fsr"):
            for mode in ("Hardware","Software"):
                for scale in (50,100):
                    self.smoke(provider,mode,scale)
        self.sample_memory()
        report, ok = perf.analyze(self.finalize(),self.tmp/"out.md",None,COMMIT,self.build)
        self.assertTrue(ok)
        self.assertIn("60.00 s",report)
        self.assertIn("| hardware-rr | V / Vulkan DLSS Ray Reconstruction",report)
        self.assertIn("Memory: **PASS**. 6,999,999,999 B",report)
        self.assertIn("Normal cadence: **PASS**",report)
        self.assertIn("Ray GPU scope: **PASS**",report)
        self.manifest["runs"] = [r for r in self.manifest["runs"] if r["id"] != "candidate-memory"]
        self.sample_memory(peak=7_000_000_000)
        report, ok = perf.analyze(self.finalize(),self.tmp/"out.md",None,COMMIT)
        self.assertFalse(ok)
        self.assertIn("Memory: **OPEN/FAIL**. 7,000,000,000 B",report)
        self.manifest["runs"] = [r for r in self.manifest["runs"] if r["id"] != "hardware-2"]
        self.start("hardware-2",second=True,clean_gap=False)
        report, ok = perf.analyze(self.finalize(),self.tmp/"out.md",None,COMMIT)
        self.assertFalse(ok)
        self.assertIn("58.00 s",report)


if __name__ == "__main__":
    unittest.main()
