#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib


SCRIPT = Path(__file__).with_name("lighter-terrain-audit.py")
SPEC = importlib.util.spec_from_file_location("audit", SCRIPT)
audit = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(audit)


def fixture(root):
    """One real-format compressed tile, four 2×2 blocks."""
    trn = bytearray(88)
    for p, n in {0: 0x54524E32, 4: 1, 20: 2, 28: 2, 32: 2, 52: 1, 64: 4, 76: 1, 80: 84}.items():
        struct.pack_into("<I", trn, p, n)
    struct.pack_into("<f", trn, 68, 2.)
    trn += struct.pack("<I", 9) + b"fixture_1"
    path = root / "fixture.trn2"
    path.write_bytes(trn)
    data = bytearray(16 + 4 * 4)
    struct.pack_into("<4I", data, 0, 0x54424B42, 6, 4, 4)
    for i in range(4):
        struct.pack_into("<I", data, 16 + i * 4, len(data))
        values = [0.] * 9
        if i == 0:
            values[4] = 5.
        raw = zlib.compress(struct.pack("<9f", *values))
        header = bytearray(36)
        struct.pack_into("<2I", header, 16, len(raw), 0)
        data += header + raw
    tile = root / "fixture_1.t2bk"
    tile.write_bytes(data)
    return path, tile


class TerrainAuditTests(unittest.TestCase):
    def test_height_grid_checks_native_antidiagonal_and_partial_last_cell(self):
        far = audit.Mesh([0., 1., 2., 3., 4., 5., 6., 7., 8., 9., 10., 11., 12., 13., 14.], 5, 3, 3)
        self.assertEqual(far.x, [0, 3, 4])
        self.assertAlmostEqual(far.height(3.5, 1.5), 11.)
        self.assertAlmostEqual(far.height(4, 2), 14.)
        # Four corners with a nonplanar quad. The mesh uses b–c, not a–d.
        self.assertEqual(audit.Mesh([0., 2., 4., 16.], 2, 2, 1).height(0.5, 0.5), 3.)

    def test_cli_distinguishes_geometry_failure_cutoff_and_missing_data(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            trn, tile = fixture(root)
            output = root / "result.json"
            def run(*args):
                done = subprocess.run([sys.executable, SCRIPT, trn, "--json", output, *args],
                                      capture_output=True, text=True)
                return done, json.loads(output.read_text())["results"][0]
            done, result = run("--stride", "4")
            self.assertEqual(done.returncode, 1, done.stderr)
            self.assertEqual(result["status"], "geometry_error")
            self.assertEqual(result["strides"]["4"]["height_grid"]["max_abs"], 5.)
            self.assertTrue(any(v["over_limit"] for v in result["strides"]["4"]["grazing"].values()))
            done, result = run()
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertEqual(result["status"], "within_tolerance")
            self.assertEqual(list(result["strides"]), ["1"])
            window = ("--cutoff-window", "0", "0", "1", "1")
            done, result = run(*window)
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertEqual(result["status"], "intentional_cutoff")
            self.assertEqual(result["cutoff_blocks"], 3)
            tile.unlink()
            done, result = run(*window)
            self.assertEqual(done.returncode, 2, done.stderr)
            self.assertEqual(result["status"], "load_failed")


if __name__ == "__main__":
    unittest.main()
