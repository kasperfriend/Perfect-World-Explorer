#!/usr/bin/env python3
"""Pin the archive census's classification rules.

The census exists so a claim like "this pack's members are understood" has
something to be checked against, so its own table is the thing that can rot.
It did: `gfx.pck`'s 1,875 `.gfx` graphs and 564 `.sgc` skill records -- the
entire reason that pack ships -- were reported as "no parser in src/" because
the table listed neither extension, and `src/gfx_file.rs` parses both.
"""
import importlib.util
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
spec = importlib.util.spec_from_file_location("census", HERE / "archive-census.py")
census = importlib.util.module_from_spec(spec)
spec.loader.exec_module(census)


class ClassificationTests(unittest.TestCase):
    def test_the_effect_containers_are_not_reported_as_unparsed(self):
        # Both are `MOXTVersion:` containers and `src/gfx_file.rs` reads them.
        for ext in ("gfx", "sgc"):
            self.assertIn(ext, census.HANDLED, ext)
            self.assertIn(ext, census.SUPPORTED, ext)
            self.assertEqual(census.HANDLED[ext][0], "MOXTVERSION: ", ext)

    def test_the_signature_sniffer_knows_the_effect_container(self):
        # A `.gfx` whose name lies, and an extensionless `MOXTVersion: 41`
        # graph, both have to come out as the effect format.
        self.assertEqual(census.sniff(b"MOXTVersion: 37\r\nDedault"), "MOXTVERSION: ")
        self.assertEqual(census.sniff(b"MOXTVersion: 4\r\nPath: x"), "MOXTVERSION: ")

    def test_specific_tags_win_over_the_generic_moxb(self):
        # Every Angelica container starts `MOXB`; a bare test first would
        # swallow skin, bone, mesh and stack files as generic models.
        self.assertEqual(census.sniff(b"MOXBIKSA"), "MOXBIKSA")
        self.assertEqual(census.sniff(b"MOXBEKSA"), "MOXBEKSA")
        self.assertEqual(census.sniff(b"MOXBDMSA"), "MOXBDMSA")
        self.assertEqual(census.sniff(b"MOXBKCTS"), "MOXBKCTS")
        self.assertEqual(census.sniff(b"MOXB\x01\x00\x00\x00"), "MOXB")

    def test_a_truncated_stub_is_not_mistaken_for_a_graph(self):
        self.assertIsNone(census.sniff(b"MOXT"))

    def test_files_with_a_printed_parser_are_listed_with_it(self):
        # Every HANDLED entry names where it is read, so a reader that is
        # removed shows up as a stale table rather than as silence.
        for ext, (_sig, where) in census.HANDLED.items():
            self.assertTrue(where, ext)

    def test_every_supported_extension_has_a_signature_rule_or_is_none(self):
        for ext in census.SUPPORTED:
            self.assertIn(ext, census.HANDLED, ext)


if __name__ == "__main__":
    unittest.main()
