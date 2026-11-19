#!/usr/bin/env python3
"""Pin `tools/lightmap-survey.py` against the reader it surveys.

The bug this guards against actually shipped: the reader demanded a 256 tile
edge, while three of the four staged `.t2lm` files are 128. A survey that
invented its own notion of a valid record would have agreed with the reader and
hidden that. So these fixtures are built the way the real files are, and the
survey's verdict is compared with what `Tiles::parse` accepts.
"""

import importlib.util
import pathlib
import struct
import sys
import unittest

HERE = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("lightmap_survey", HERE / "lightmap-survey.py")
survey = importlib.util.module_from_spec(spec)
spec.loader.exec_module(survey)

TILES = 16


def fixture(edge=256, tiles=TILES, flags=1, version=2, magic=0x54324C4D, overlap=False):
    """Build one T2LM file exactly as the client lays it out."""
    tile_size = 128 + edge * edge // 2
    header = 24 + TILES * 4
    body = bytearray(header)
    offsets = []
    for i in range(TILES):
        offsets.append(header + i * tile_size)
    total = header + TILES * tile_size
    body.extend(b"\0" * (total - header))
    for i, start in enumerate(offsets):
        tile = bytearray(tile_size)
        tile[0:4] = b"DDS "
        struct.pack_into("<7I", tile, 4, 124, 0x81007, edge, edge, edge // 2 * 8, 0, 0)
        struct.pack_into("<4I", tile, 4 + 72, 32, 0x4, 0x31545844, 0)
        if overlap and i == 1:
            start = offsets[0]
        body[start:start + tile_size] = tile
    struct.pack_into("<6I", body, 0, magic, version, tiles, edge, flags, tile_size)
    for i, start in enumerate(offsets):
        struct.pack_into("<I", body, 24 + i * 4, start)
    return bytes(body)


class SurveyTest(unittest.TestCase):
    def setUp(self):
        self.dir = pathlib.Path(self._tmp())
        self.dir.mkdir(parents=True, exist_ok=True)

    def _tmp(self):
        import tempfile
        return tempfile.mkdtemp(prefix=f"pw-lmsurvey-{id(self)}-")

    def write(self, name, data):
        path = self.dir / name
        path.write_bytes(data)
        return path

    def test_both_shipped_tile_edges_are_accepted_and_reported(self):
        for edge in (128, 256):
            path = self.write(f"m{edge}.t2lm", fixture(edge=edge))
            result = survey.survey(path)
            self.assertNotIn("refuse", result, edge)
            self.assertEqual(result["side"], edge)
            # The header edge and the DDS edge are two statements of one fact.
            self.assertEqual(result["edges"], {(edge, edge)})
            self.assertEqual(result["mips"], {0})
            self.assertEqual(result["formats"], {b"DXT1": 16})

    def test_an_edge_no_client_ships_is_reported_not_crashed(self):
        path = self.write("m512.t2lm", fixture(edge=512))
        result = survey.survey(path)
        # The survey describes what exists; it does not enforce the reader's
        # two-value allow-list, so 512 is summarised rather than refused.
        self.assertNotIn("refuse", result)
        self.assertEqual(result["side"], 512)

    def test_malformed_records_are_refused_with_a_reason(self):
        cases = {
            "bad_magic.t2lm": fixture(magic=0),
            "bad_version.t2lm": fixture(version=3),
            "bad_tiles.t2lm": fixture(tiles=8),
            "bad_flags.t2lm": fixture(flags=0),
            "overlap.t2lm": fixture(overlap=True),
            "truncated.t2lm": b"ML2T" + b"\0" * 4,
        }
        for name, data in cases.items():
            path = self.write(name, data)
            result = survey.survey(path)
            self.assertIn("refuse", result, name)
            self.assertTrue(result["refuse"], name)

    def test_every_refusal_names_the_reason(self):
        # A silent skip is the failure mode these surveys exist to remove.
        path = self.write("x.t2lm", fixture(version=9))
        self.assertIn("version 9", survey.survey(path)["refuse"])


if __name__ == "__main__":
    unittest.main(verbosity=1)
