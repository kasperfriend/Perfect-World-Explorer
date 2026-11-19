#!/usr/bin/env python3
"""Pin `tools/ecbsd-survey.py` to the rules `src/grass.rs` actually applies.

The survey's first job is to measure `ECBSD_NAME_LIMIT`. A survey that invented
its own idea of a valid table would report a maximum the reader disagrees with,
so each refusal here mirrors a specific check in `water::path_string` or in
`grass::grass_names`.
"""

import importlib.util
import pathlib
import struct
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("ecbsd_survey", HERE / "ecbsd-survey.py")
survey = importlib.util.module_from_spec(spec)
spec.loader.exec_module(survey)

HEADER = 0x4C


def fixture(names, version=18, identity=survey.IDENTITY, count=None):
    body = bytearray(HEADER)
    struct.pack_into("<2I", body, 0, *identity)
    struct.pack_into("<2I", body, 8, version, len(names) if count is None else count)
    for name in names:
        raw = name.encode("gbk")
        body += struct.pack("<I", len(raw)) + raw
    return bytes(body)


class SurveyTest(unittest.TestCase):
    def setUp(self):
        self.dir = pathlib.Path(tempfile.mkdtemp(prefix="pw-ecbsd-"))

    def survey(self, data, name="m.ecbsd"):
        path = self.dir / name
        path.write_bytes(data)
        return survey.survey(path)

    def test_the_largest_real_table_reads(self):
        # 88 is the measured maximum (`world.ecbsd`), and it must be ordinary.
        names = [f"BSData/{i}.dat" for i in range(1, 89)]
        result = self.survey(fixture(names))
        self.assertNotIn("refuse", result)
        self.assertEqual(result["count"], 88)
        self.assertEqual(result["names"][0], "BSData/1.dat")
        self.assertEqual(result["names"][-1], "BSData/88.dat")

    def test_both_shipped_revisions_read(self):
        for version in (17, 18):
            result = self.survey(fixture(["BSData/1.dat"], version=version))
            self.assertNotIn("refuse", result, version)
            self.assertEqual(result["version"], version)

    def test_a_foreign_identity_is_refused_by_name(self):
        result = self.survey(fixture(["BSData/1.dat"], identity=(0, 0)))
        self.assertIn("identity", result["refuse"])
        self.assertIn("0x00000000", result["refuse"])

    def test_a_short_header_is_refused_not_crashed(self):
        self.assertIn("shorter than", self.survey(b"BS")["refuse"])

    def test_count_beyond_the_file_is_refused(self):
        result = self.survey(fixture(["BSData/1.dat"], count=10_000))
        self.assertIn("10000 names", result["refuse"])

    def test_each_name_refusal_names_the_index_and_reason(self):
        cases = {
            "truncated": b"BSData/1",
            "too long": b"\xff\xff\xff\x7f" + b"",  # 2^31-1 length
            "unsafe": struct.pack("<I", 9) + b"/etc/pass",
        }
        for label, tail in cases.items():
            data = bytearray(fixture([], count=1)) + bytearray(tail)
            result = self.survey(bytes(data), name=f"{label.replace(' ', '_')}.ecbsd")
            self.assertIn("name 0 of 1", result["refuse"], label)


if __name__ == "__main__":
    unittest.main(verbosity=1)
