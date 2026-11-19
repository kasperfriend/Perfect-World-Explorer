import hashlib
from pathlib import Path
import tempfile
import unittest

from PIL import Image

import lighter_capture_matrix as matrix
import lighter_temporal_compare as temporal


class CaptureMatrixTests(unittest.TestCase):
    def test_region_mean_absolute_error_ignores_alpha_and_uses_native_pixels(self):
        with Image.new("RGBA", (12, 12), (10, 20, 30, 255)) as a:
            b = a.copy()
            b.paste((40, 40, 40, 127), (0, 0, 12, 4))
            report, _ = matrix.compare(a, b)
        self.assertEqual(report["upper"]["mean_abs_rgb8"], 20)
        self.assertEqual(report["upper"]["channel_mean_abs_rgb8"], [30, 20, 10])
        self.assertEqual(report["whole"]["max_abs_rgb8"], 30)
        self.assertEqual(report["whole"]["channel_max_abs_rgb8"], [30, 20, 10])
        self.assertEqual(report["lower"]["max_abs_rgb8"], 0)
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

    def test_water_needs_actual_lightmap_admission_on_both_routes(self):
        case = {"lightmaps": True, "min_lightmap_areas_day_night": 16}
        meta = {"native_lightmap_areas_day_night": "16"}
        matrix.validate_lightmap_admission(meta, "off", case, "water-off")
        for ray in ("software", "hardware"):
            with self.assertRaisesRegex(ValueError, "16 bound authored"):
                matrix.validate_lightmap_admission(meta, ray, case, "water-ray")
            matrix.validate_lightmap_admission(meta | {"ray_lightmap_areas_bound": "16"}, ray, case, "water-ray")

    def test_missing_inventory_class_is_not_complete(self):
        with tempfile.TemporaryDirectory() as tmp:
            stem = Path(tmp) / "partial"
            stem.with_suffix(".coverage.csv").write_text(",".join(matrix.INVENTORY_COLUMNS) + "\nnear_terrain,1," + ",".join(["0"] * 14) + "\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "missing inventory rows"):
                matrix.inventory_for(stem, {})

    def test_native_inventory_binds_csv_and_reconciles_totals(self):
        with tempfile.TemporaryDirectory() as tmp:
            stem = Path(tmp) / "fixture"
            raw = (",".join(matrix.INVENTORY_COLUMNS) + "\n"
                   + "".join(f"{name},{int(name == 'near_terrain')}," + ",".join(["0"] * 14) + "\n"
                             for name in ("near_terrain", "far_terrain", "trees", "grass", "buildings", "water",
                                          "world_models", "walker", "populator", "effect_draws", "effect_roots")))
            stem.with_suffix(".coverage.csv").write_bytes(raw.encode())
            meta = dict.fromkeys(("native_authored_bmds", "native_authored_water_areas", "native_authored_tree_placements",
                                  "native_authored_world_actors", "native_effect_quads", "world_effect_authored_scene_roots",
                                  "world_effect_decoded_model_event_roots", "grass_authored_areas"), "0")
            meta |= {f"grass_areas_{k}": "0" for k in (*matrix.TOTALS, "range", "empty", "cutoff", "budget", "disabled", "unavailable")}
            meta |= {"native_inventory_scope": matrix.NATIVE_SCOPE, "grass_inventory_scope": "native_stream_only",
                     "inventory_sha256": hashlib.sha256(raw.encode()).hexdigest()}
            self.assertEqual(matrix.capture_inventory(stem, "off", meta, {})["near_terrain"]["loaded"], 1)
            with self.assertRaisesRegex(ValueError, "Native grass area"):
                matrix.capture_inventory(stem, "off", meta | {"grass_authored_areas": "1"}, {})
            self.assertIsNone(matrix.capture_inventory(stem, "off", meta | {"native_inventory_scope": "partial"}, {}))
            stem.with_suffix(".coverage.csv").write_bytes(raw.encode() + b"\n")
            with self.assertRaisesRegex(ValueError, "inventory SHA-256"):
                matrix.capture_inventory(stem, "off", meta, {})

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

    def test_temporal_receipts_reject_fallback_unmatched_path_and_unrefined_reference(self):
        shared = dict.fromkeys(matrix.MATCH_KEYS, "fixed") | {
            "platform": "windows",
            "adapter": 'name: "NVIDIA GeForce RTX 3080", vendor: 4318, device_type: DiscreteGpu, backend: Vulkan',
            "mode": "Software", "warmup_frames": "48", "warmup_step_ms": "16", "animation_elapsed_ms": "768",
            "raster_primary": "false", "screen_reuse_flags": "0", "camera_path_sha256": "a" * 64,
        }
        rays = "Settings { mode: Software, samples: 1, reference: false, dynamic_resolution: false }"
        ref = ("Settings { mode: Software, samples: %d, reference: true, dynamic_resolution: false, "
               "bounces: 8, resolution: 100, progressive: false, denoiser: false }")
        metas = {key: shared | {"ray_reconstruction": provider, "settings_effective": rays}
                 for key, provider in temporal.PROVIDERS.items()}
        metas["reference_low"]["settings_effective"] = ref % 32
        metas["reference"]["settings_effective"] = ref % 64
        temporal.validate_temporal(metas, "reveal")
        with self.assertRaisesRegex(ValueError, "requested.*got portable"):
            temporal.validate_temporal(metas | {"rr": metas["rr"] | {"ray_reconstruction": "portable"}}, "reveal")
        with self.assertRaisesRegex(ValueError, "unmatched camera_path_sha256"):
            temporal.validate_temporal(metas | {"fsr": metas["fsr"] | {"camera_path_sha256": "b" * 64}}, "reveal")
        with self.assertRaisesRegex(ValueError, "doubled budget"):
            temporal.validate_temporal(metas | {"reference": metas["reference"] | {"settings_effective": ref % 32}}, "reveal")

    def test_rt17_ignores_cpu_pairs_and_identifies_incomplete_native_counts(self):
        cases = [{"id": "wet", "map": "a07", "classes": ["water"], "condition": "day",
                  "camera": [16, 356, 84, 0, -0.6], "camera_evidence": "review/a07.png"}]
        timing = {"samples": [1, 2, 3]}
        meta = {"platform": "windows", "architecture": "x86_64",
                "adapter": 'name: "NVIDIA GeForce RTX 3080", vendor: 4318, device_type: DiscreteGpu, backend: Vulkan',
                "vulkan_largest_device_local_heap_bytes": str(10 << 30),
                "trace_image": "480x360", "inventory_sha256": "a" * 64}
        sample = {"inventory": {"water": {}}, "synchronized_capture_ms": timing, "meta": meta}
        captures = {"off": sample | {"inventory": None}, "software": sample, "hardware": sample}
        pins = dict.fromkeys(matrix.SHARED_INPUTS | {f"maps/a07/a07{suffix}" for suffix in
                            (".ecwld", ".ecbsd", ".trn2", "_1.t2bk", "_1.t2mk")}, "b" * 64)
        report = {"profile": "physical", "cases": [{"case": cases[0], "captures": captures}],
                  "errors": {}, "client_inputs": pins}
        physical = matrix.missing_rt17_cells(cases, report)
        self.assertEqual(len(physical), 52)
        self.assertTrue(any(c["route"] == "off" and c["scene_class"] == "water"
                            and "Native" in c["reason"] for c in physical))
        self.assertEqual(len(matrix.missing_rt17_cells(cases, report | {"profile": "cpu"})), 54)
        self.assertEqual(len(matrix.missing_rt17_cells(cases, report | {"client_inputs": {}})), 54)
        self.assertEqual(len(matrix.missing_rt17_cells([cases[0] | {"camera_evidence": None}], report)), 54)
        self.assertTrue(matrix.requested_target(meta))
        self.assertFalse(matrix.requested_target(meta | {"vulkan_largest_device_local_heap_bytes": str(12 << 30)}))
        self.assertFalse(matrix.requested_target(meta | {"platform": "linux"}))

    def test_input_hashes_refuse_mixed_clients_and_path_traversal(self):
        with tempfile.TemporaryDirectory() as tmp:
            client = Path(tmp)
            (client / "element/maps").mkdir(parents=True)
            payload = client / "element/textures.pck"
            payload.write_bytes(b"exact bytes")
            good = hashlib.sha256(b"exact bytes").hexdigest()
            pins = client / "pins.txt"
            pins.write_text(f"# pins\n{good}  textures.pck\n", encoding="utf-8")
            self.assertEqual(matrix.verify_inputs(client, pins), {"textures.pck": good})
            payload.write_bytes(b"same length")
            with self.assertRaisesRegex(ValueError, "input SHA-256 mismatch"):
                matrix.verify_inputs(client, pins)
            pins.write_text(f"{good} ../textures.pck\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "invalid or duplicate"):
                matrix.verify_inputs(client, pins)

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
