import hashlib
from pathlib import Path
import tempfile
import unittest

from PIL import Image

import lighter_capture_matrix as matrix


class CaptureMatrixTests(unittest.TestCase):
    def test_region_mean_absolute_error_ignores_alpha_and_uses_native_pixels(self):
        with Image.new("RGBA", (12, 12), (10, 20, 30, 255)) as a:
            b = a.copy()
            b.paste((40, 40, 40, 127), (0, 0, 12, 4))
            report, _ = matrix.compare(a, b)
        self.assertEqual(report["upper"]["mean_abs_rgb8"], 20)
        self.assertEqual(report["upper"]["channel_mean_abs_rgb8"], [30, 20, 10])
        self.assertEqual(report["whole"]["mean_abs_rgb8"], 6.66667)
        self.assertEqual(report["lower"]["mean_abs_rgb8"], 0)
        with self.assertRaisesRegex(ValueError, "invalid region"):
            list(matrix.regions_for((10, 10), {"bad": (.8, 0, .1, 1)}))

    def test_mismatched_camera_and_ray_settings_are_refused(self):
        common = dict.fromkeys(matrix.MATCH_KEYS, "fixed")
        soft = common | {"mode": "Software", "settings_effective": "Settings { mode: Software, samples: 1 }"}
        hard = common | {"mode": "Hardware", "settings_effective": "Settings { mode: Hardware, samples: 1 }"}
        matrix.validate_match(soft, hard)
        with self.assertRaisesRegex(ValueError, "unmatched eye"):
            matrix.validate_match(soft, hard | {"eye": "different"})
        with self.assertRaisesRegex(ValueError, "different effective settings"):
            matrix.validate_match(soft, hard | {"settings_effective": hard["settings_effective"].replace("1", "2")})
        for name in ("bc_terrain_tiles", "bc_terrain_saved_bytes", "screen_reuse_flags"):
            with self.assertRaisesRegex(ValueError, f"unmatched {name}"):
                matrix.validate_match(soft, hard | {name: "3"})
            matrix.validate_match(soft | {name: "3"}, hard | {name: "3"})

    def test_hybrid_ab_relaxes_only_the_primary_toggle(self):
        full = dict.fromkeys(matrix.MATCH_KEYS, "fixed") | {
            "mode": "Hardware", "settings_effective": "Settings { mode: Hardware, hybrid_primary: false, samples: 1 }",
        }
        hybrid = full | {"settings_effective": full["settings_effective"].replace("false", "true")}
        matrix.validate_match(full, hybrid, compare_primary=True)
        with self.assertRaisesRegex(ValueError, "different effective settings"):
            matrix.validate_match(full, hybrid)
        with self.assertRaisesRegex(ValueError, "different effective settings"):
            matrix.validate_match(full, hybrid | {"settings_effective": hybrid["settings_effective"].replace("1", "2")},
                                  compare_primary=True)

    def test_completed_stages_are_named_and_bounded_by_timed_draws(self):
        meta = {"gpu_timed_stages": '"build,primary,traversal,reproject,filter,upscale"',
                "gpu_timed_ms": "[[0,2,10,1,1.4,0.8],[4,4,8,2,2.6,1.2]]"}
        self.assertEqual(matrix.completed_stages(meta, 2)["p50"], {
            "build": 2, "primary": 3, "traversal": 9, "reproject": 1.5, "filter": 2, "upscale": 1,
        })
        with self.assertRaisesRegex(ValueError, "invalid completed GPU stage samples"):
            matrix.completed_stages(meta, 1)
        self.assertIsNone(matrix.completed_stages({}, 9))

    def test_physical_profile_cannot_use_cpu_and_digest_binds_image(self):
        with tempfile.TemporaryDirectory() as tmp:
            stem = Path(tmp) / "water"
            image = Image.new("RGBA", (9, 9), (255, 255, 255, 255))
            path = stem.with_suffix(".png")
            image.save(path)
            sidecar = stem.with_suffix(".coverage.txt")
            sidecar.write_text(
                'coverage_schema=3\nmap="a07"\nmode=Hardware\nhour=12\nweather=Clear\nwarmup_frames=12\nlightmaps=false\n'
                'animation_elapsed_ms=192\nwarmup_step_ms=16\nrain_at_eye=false\n'
                'image=9x9\nadapter=AdapterInfo { device_type: Cpu, backend: Vulkan }\n'
                f'rgba8_sha256={hashlib.sha256(image.tobytes()).hexdigest()}\n', encoding="utf-8"
            )
            case = {"map": "a07", "condition": "day", "warmup": 12, "lightmaps": False}
            with self.assertRaisesRegex(ValueError, "physical evidence cannot use Cpu"):
                matrix.capture_metadata(stem, "hardware", case, "physical")
            paper = sidecar.read_text(encoding="utf-8").replace("Cpu", "DiscreteGpu")
            sidecar.write_text(paper, encoding="utf-8")
            matrix.capture_metadata(stem, "hardware", case, "physical")
            sidecar.write_text(paper + "ray_reconstruction=Vulkan DLSS Ray Reconstruction\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "requires portable ray reconstruction"):
                matrix.capture_metadata(stem, "hardware", case, "physical")
            sidecar.write_text(paper, encoding="utf-8")
            image.putpixel((3, 2), (255, 0, 0, 255))
            image.save(path)
            with self.assertRaisesRegex(ValueError, "RGBA8 hash disagrees"):
                matrix.capture_metadata(stem, "hardware", case, "physical")

    def test_gpu_stages_keep_gpu_time_separate_from_capture_wall_time(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = Path(tmp) / "capture.log"
            log.write_text(
                "Lighter gpu stage Software: traversal_ms=10.200 filter_ms=1.900\n"
                "Lighter gpu stage Vulkan: build_ms=1.100 traversal_ms=2.300 allocation_bytes=10 budget_bytes=100\n"
                "Lighter gpu stage Hardware: traversal_ms=raw-vulkan filter_ms=0.500\n",
                encoding="utf-8",
            )
            a, b, c = matrix.gpu_stage_samples(log)
        self.assertEqual(a, {"stage_source": "Software", "traversal_ms": 10.2, "filter_ms": 1.9})
        self.assertEqual(b["allocation_bytes"], 10)
        self.assertEqual(c, {"stage_source": "Hardware", "filter_ms": 0.5})


if __name__ == "__main__":
    unittest.main()
