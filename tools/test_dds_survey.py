#!/usr/bin/env python3
"""Pin the DDS survey's classification against the reader it describes.

`tools/dds-survey.py` exists to say which DDS pixel formats a pack carries and
whether `src/model_dds.rs` reads them. Its first version got that second half
wrong: it reported the 33 24-bit members as unreadable when the parser has
always accepted 24-bit BGR. A survey that measures its own table instead of the
code reports gaps that are not there -- the same failure the `gfx.pck` census
had, which is why both tables now have tests.

These tests are synthetic: they build headers in memory, so CI needs no corpus.
"""
import importlib.util
import struct
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("dds_survey", HERE / "dds-survey.py")
survey = importlib.util.module_from_spec(spec)
spec.loader.exec_module(survey)


def dds(pf_flags, bit_count=0, masks=(0, 0, 0, 0), fourcc=b"\0\0\0\0", size=256):
    """A 128-byte DDS header plus `size` payload bytes."""
    b = bytearray(128)
    b[0:4] = b"DDS "
    struct.pack_into("<I", b, 4, 124)
    struct.pack_into("<I", b, 76, 32)
    struct.pack_into("<I", b, 80, pf_flags)
    b[84:88] = fourcc
    struct.pack_into("<I", b, 88, bit_count)
    struct.pack_into("<IIII", b, 92, *masks)
    struct.pack_into("<I", b, 12, 8)  # height
    struct.pack_into("<I", b, 16, 8)  # width
    struct.pack_into("<I", b, 28, 4)  # mip count
    return bytes(b) + bytes(size)


class ClassificationTests(unittest.TestCase):
    def test_the_three_compressed_formats_the_packs_carry(self):
        for tag in (b"DXT1", b"DXT3", b"DXT5"):
            kind, detail = survey.classify(dds(0x4, fourcc=tag))
            self.assertEqual((kind, detail), ("fourcc", tag.decode()))
            self.assertIn(detail, survey.READ)

    def test_an_unknown_fourcc_is_reported_unreadable_not_dropped(self):
        kind, detail = survey.classify(dds(0x4, fourcc=b"DX10"))
        self.assertEqual((kind, detail), ("fourcc", "DX10"))
        # `DX10` has no header path in `model_dds.rs`; the survey must say so
        # rather than assume a FourCC tag is enough.
        self.assertNotIn("DX10", survey.READ)

    def test_every_uncompressed_layout_the_parser_accepts_is_readable(self):
        """The list is checked against `model_dds.rs`, not against a pack.

        `model_dds.rs` accepts 32-bit BGRA (with or without an alpha mask),
        16-bit RGB565 and 24-bit BGR. Reporting any of them as unreadable is
        the bug this test exists to prevent.
        """
        layouts = [
            (32, (0xFF0000, 0xFF00, 0xFF, 0xFF000000)),
            (32, (0xFF0000, 0xFF00, 0xFF, 0)),
            (16, (0x7C00, 0x3E0, 0x1F, 0)),
            (24, (0xFF0000, 0xFF00, 0xFF, 0)),
        ]
        for bits, masks in layouts:
            kind, detail = survey.classify(dds(0x40, bits, masks))
            self.assertEqual(kind, "rgb")
            self.assertIn(
                detail,
                survey.READ_RGB,
                f"{bits}-bit layout {masks} is readable by the parser but the "
                f"survey does not list it",
            )

    def test_luminance_is_one_channel_and_readable_at_8_and_16_bits(self):
        # The staged `textures.pck` caustic set: 16 bits, masks 000000ff/0000ff00.
        kind, detail = survey.classify(dds(0x80000, 16, (0xFF, 0xFF00, 0, 0)))
        self.assertEqual((kind, detail), ("luminance", "16-bit"))
        self.assertIn(16, survey.READ_LUMINANCE)
        # A width the parser refuses must not be advertised as read.
        self.assertNotIn(4, survey.READ_LUMINANCE)

    def test_luminance_is_tested_before_rgb(self):
        """Both bits can be set at once; luminance is the narrower reading.

        A header with `DDPF_RGB | DDPF_LUMINANCE` describes one channel, and the
        parser branches on luminance first. A classifier that tested RGB first
        would call it a colour surface and mis-describe the data.
        """
        kind, _ = survey.classify(dds(0x40 | 0x80000, 16, (0xFF, 0xFF00, 0, 0)))
        self.assertEqual(kind, "luminance")

    def test_a_payload_that_is_not_a_dds_is_reported_as_such(self):
        self.assertEqual(survey.classify(b"")[0], None)
        self.assertEqual(survey.classify(b"XXXX" + bytes(200))[0], None)
        self.assertEqual(survey.classify(b"DDS " + bytes(10))[0], None)

    def test_an_unknown_pixel_format_flags_word_is_reported(self):
        kind, detail = survey.classify(dds(0x2000, 8))
        self.assertEqual(kind, "other")
        self.assertIn("0x2000", detail)


if __name__ == "__main__":
    unittest.main()
