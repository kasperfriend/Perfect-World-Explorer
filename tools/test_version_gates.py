"""Regression tests for the file-specific version offsets in version-gates.py."""

import os
import struct
import subprocess
import sys
import tempfile
import unittest


SCRIPT = os.path.join(os.path.dirname(__file__), "version-gates.py")


class VersionGateSurveyTests(unittest.TestCase):
    def test_tasks_magic_is_not_reported_as_its_version(self):
        with tempfile.TemporaryDirectory(prefix="pwe-version-gates-") as root:
            def write(relative, words):
                path = os.path.join(root, relative)
                os.makedirs(os.path.dirname(path), exist_ok=True)
                with open(path, "wb") as handle:
                    handle.write(struct.pack("<" + "I" * len(words), *words))
                    handle.write(bytes(32))

            write("elements.data", [0x3000001B, 0x4B39C931])
            write("tasks.data", [0x93858361, 75])
            write("aipolicy.data", [0x1234, 7])

            # These .data files carry non-version header words and must not be
            # swept into the generic elements/tasks version bucket.
            write("domain.data", [0x42584F4D, 2])
            write("gshop.data", [0x5CCC5F37, 0])
            write("task_npc.data", [4096, 0x12345678, (42 << 16) | 9])
            write("dyn_tasks.data", [4096, 0x12345678, 7])

            # A wrong task magic must be named and excluded, not counted as a
            # plausible version number.
            write("regional/tasks.data", [0xDEADBEEF, 76])

            result = subprocess.run(
                [sys.executable, SCRIPT, root],
                check=True,
                capture_output=True,
                text=True,
            )
            output = result.stdout

            self.assertIn("elements data: 1 file(s)", output)
            self.assertIn("family 0x3000  version 27", output)
            self.assertIn("tasks data: 1 file(s)", output)
            self.assertIn("version 75", output)
            self.assertIn("AI policy trigger: 1 file(s)", output)
            self.assertIn("version 7", output)
            self.assertIn("tasks.data: 1 file(s) with magic 0xdeadbeef", output)
            self.assertNotIn("version 2474998625", output)  # 0x93858361
            self.assertNotIn("version 76", output)
            self.assertNotIn("domain data:", output)
            self.assertNotIn("task_npc data:", output)
            self.assertNotIn("dyn_tasks data:", output)


if __name__ == "__main__":
    unittest.main()
