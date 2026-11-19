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
import pck_lib  # noqa: E402 - after the path insert above


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

    def test_an_enciphered_package_is_skipped_not_reported_damaged(self):
        """A sealed archive is a feature gap, not a broken file.

        `pck_lib` raises `EncryptedPackage` for `dwFlags` bit 31 so a survey can
        say "this build lacks the feature" instead of blaming the archive.
        """
        import io
        import struct
        import tempfile

        pck_lib = census.pck
        # A trailer that passes the guards and states 0x00020002, with bit 31 set
        # in `dwFlags` (+12 for that revision).
        trailer = bytearray(280)
        struct.pack_into("<I", trailer, 0, 0xFDFDFEEE)
        struct.pack_into("<I", trailer, 12, 0x84030201)
        struct.pack_into("<I", trailer, 268, 0xF00DBEEF)
        struct.pack_into("<I", trailer, 272, 0)
        struct.pack_into("<I", trailer, 276, 0x00020002)
        body = struct.pack("<III", 0x4DCA23EF, 0, 0x56A089B7) + bytes(trailer)
        body = body[:4] + struct.pack("<I", len(body)) + body[8:]

        with tempfile.NamedTemporaryFile(suffix=".pck", delete=False) as fh:
            fh.write(body)
            path = fh.name
        try:
            with self.assertRaises(pck_lib.EncryptedPackage) as caught:
                pck_lib.directory(path)
            self.assertIn("encrypted package unsupported", str(caught.exception))
            # And the census turns it into a skip with no rows, not a crash.
            version, rows = census.census(path)
            self.assertIsNone(version)
            self.assertEqual(rows, [])
        finally:
            Path(path).unlink()

    def test_the_encrypted_error_is_distinct_from_a_parse_failure(self):
        # `EncryptedPackage` must not be a subclass whose `except Exception`
        # handler would swallow it somewhere it should surface; it is a plain
        # Exception and surveys catch it by name.
        self.assertTrue(issubclass(census.pck.EncryptedPackage, Exception))
        self.assertFalse(issubclass(census.pck.EncryptedPackage, AssertionError))


if __name__ == "__main__":
    unittest.main()


class RevisionLayoutTests(unittest.TestCase):
    """`dwFlags` and the record geometry move with the revision.

    Reading `dwFlags` at a fixed +12 reports a 1.8.4 package as enciphered,
    because at that offset revision `0x00020003` carries the first bytes of
    `szDescription` (`0xffffffff`). The 1.5.2 and 1.8.4 trailers also differ in
    length, so a fixed `end - 280` reads the wrong place entirely.
    """

    def _package(self, revision, entries=1, flags=0):
        import struct

        (
            header_size,
            entry_size,
            offset_at,
            offset_width,
            size_at,
            comp_at,
            guard_byte1,
            flags_at,
        ) = pck_lib.REVISIONS[revision]
        del offset_at, offset_width, size_at, comp_at
        payload = b"\0\0\0\0"
        body = bytearray(struct.pack("<III", 0x4DCA23EF, 0, 0x56A089B7))
        body += b"\0" * (12 * entries)
        body += b"\0" * (header_size + 8)
        size = len(body)
        struct.pack_into("<I", body, 4, size + len(payload))
        trailer = size - (header_size + 8)
        struct.pack_into("<I", body, trailer, 0xFDFDFEEE)
        struct.pack_into("<I", body, trailer + flags_at, flags)
        struct.pack_into("<I", body, trailer + guard_byte1, 0xF00DBEEF)
        struct.pack_into("<I", body, trailer + header_size, 0)
        struct.pack_into("<I", body, size - 4, revision)
        return bytes(body)

    def test_flags_are_read_at_the_revisions_own_offset(self):
        import tempfile
        from pathlib import Path

        # Unencrypted must not be reported as enciphered at either revision.
        for revision in (0x20002, 0x20003):
            data = self._package(revision)
            with tempfile.NamedTemporaryFile(suffix=".pck", delete=False) as fh:
                fh.write(data)
                path = fh.name
            try:
                # Whatever the container parse does, it must not be the
                # *encryption* refusal.
                try:
                    pck_lib.directory(path)
                except pck_lib.EncryptedPackage:
                    self.fail(f"revision {revision:08x} is not enciphered")
                except Exception:
                    pass  # a synthetic zero-entry package may not parse, fine
            finally:
                Path(path).unlink()

        # And bit 31 at the right offset *is* reported.
        for revision in (0x20002, 0x20003):
            data = self._package(revision, flags=0x80000000)
            with tempfile.NamedTemporaryFile(suffix=".pck", delete=False) as fh:
                fh.write(data)
                path = fh.name
            try:
                with self.assertRaises(pck_lib.EncryptedPackage, msg=f"{revision:08x}"):
                    pck_lib.directory(path)
            finally:
                Path(path).unlink()

    def test_every_revision_names_its_flags_offset(self):
        # +12 for the 272-byte trailer, +16 for the 280-byte one. A revision
        # added later must state it rather than inherit a default.
        self.assertEqual(pck_lib.REVISIONS[0x20001][-1], 12)
        self.assertEqual(pck_lib.REVISIONS[0x20002][-1], 12)
        self.assertEqual(pck_lib.REVISIONS[0x20003][-1], 16)
        # And the record geometry differs, which is why the container is chosen
        # from the revision instead of assumed.
        self.assertEqual(pck_lib.REVISIONS[0x20003][1], 288)
        self.assertEqual(pck_lib.REVISIONS[0x20002][1], 276)
