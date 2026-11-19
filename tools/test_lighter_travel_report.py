import tempfile
from pathlib import Path
import unittest

from lighter_travel_report import STAGES, capture, summarize, validate_pair, validate_suite


class TravelTest(unittest.TestCase):
    def write(self, dir, mode="Hardware", name="control.csv", frames=30, pose_shift=0,
              sync=False, provider="portable"):
        file = Path(dir) / name
        headers = dict(schema=1, count=max(30, frames), map="a07", hour=12, warmup=20, plan_sha256="a"*64,
                       display="960x720", world="960x720", trace="480x360", present_mode="Fifo", view="clear",
                       view_sha256="b"*64, screen_cache_mask=0, compact_terrain="false", sampled_bc="false",
                       hybrid=str(name == "hybrid.csv").lower(),
                       adapter='AdapterInfo { name: "Fixture", device_type: Cpu, backend: Vulkan }',
                       ray=f"Settings {{ mode: {mode}, hybrid_primary: {str(name == 'hybrid.csv').lower()}, resolution: 50 }}")
        if sync:
            headers.update(capture_policy="synchronized", ray_reconstruction=provider,
                           peak_sampled_device_local_heap_estimate_bytes=7_000_000_001)
        data = [f"# {k}={v}\n" for k,v in headers.items()]
        data += ["frame,interval_ms,redraw_ms,acquire_ms,present_call_ms,x,y,z,yaw,pitch,chunk_x,chunk_z,streaming,committed,device_local_heap_estimate_bytes"]
        if sync:
            data[-1] += "," + ",".join(STAGES)
        data[-1] += "\n"
        for i in range(frames):
            interval = 40 if i == 20 else 16
            memory = "12345" if i % 10 == 0 else ""
            row = f"{i},{interval},6,0.2,0.1,{i+pose_shift},2,3,0,-0.4,0,0,{int(i == 19)},{int(i == 20)},{memory}"
            if sync:
                row += ",1.5,,,8,,,0.2," + ("2" if provider != "portable" else "")
            data.append(row + "\n")
        file.write_text(''.join(data), encoding="utf-8")
        return file

    def test_reports_stream_tails_and_optional_heap_but_rejects_cpu_as_physical(self):
        with tempfile.TemporaryDirectory() as directory:
            run = capture(self.write(directory))
            report = summarize(run, require_commit=True, require_heap=True)
            self.assertEqual(report["normal_present_calls"]["p50_ms"], 16)
            self.assertEqual(report["normal_present_calls"]["slowest_one_percent_fps"], 25.)
            self.assertEqual(report["streaming_or_commit_intervals"]["n"], 2)
            self.assertEqual(report["peak_sampled_device_local_heap_estimate_bytes"], 12345)
            with self.assertRaisesRegex(ValueError, "physical Vulkan"):
                summarize(run, physical=True)

    def test_counts_and_pose_matching(self):
        with tempfile.TemporaryDirectory() as directory:
            base = capture(self.write(directory))
            validate_pair(base, capture(self.write(directory, name="hybrid.csv")))
            with self.assertRaisesRegex(ValueError, "camera paths"):
                validate_pair(base, capture(self.write(directory, name="changed.csv", pose_shift=1)))
            with self.assertRaisesRegex(ValueError, "incomplete"):
                capture(self.write(directory, name="partial.csv", frames=29))

    def test_synchronized_measurements_require_complete_count_and_scope(self):
        with tempfile.TemporaryDirectory() as directory:
            path = self.write(directory, frames=500, sync=True, provider="Vulkan temporal FSR")
            with self.assertRaisesRegex(ValueError, "normal_present"):
                capture(path)
            record = capture(path, synchronized=True)
            report = summarize(record)
            self.assertEqual(report["synchronized_present_calls"]["n"], 500)
            self.assertEqual(report["synchronized_stages"]["gpu_sdk_ms"]["p50_ms"], 2)
            self.assertTrue(report["heap_at_or_above_7_gb"])
            self.assertIsNone(report["synchronized_stages"]["gpu_as_ms"])
            with self.assertRaisesRegex(ValueError, "missing or duplicate synchronized routes"):
                validate_suite([record])
            with self.assertRaisesRegex(ValueError, "incomplete"):
                capture(self.write(directory, frames=499, sync=True), synchronized=True)

    def test_motion_trace_cannot_enter_either_benchmark_report(self):
        with tempfile.TemporaryDirectory() as directory:
            path = self.write(directory)
            path.write_text("# capture_policy=motion_readback\n" + path.read_text())
            for sync in (False, True):
                with self.assertRaisesRegex(ValueError, "exclude motion/readback"):
                    capture(path, synchronized=sync)
