#!/usr/bin/env python3
"""Fail when `tools/offline_harness.sh` has a dead `case` arm.

A shell `case` takes its first match. Two arms carrying the same label is not a
syntax error and it prints nothing at all: the second arm simply never runs, and
every mode that was supposed to reach it silently does something else. That is
not hypothetical. This script exists because two arms were labelled `music` —
one was a `precinct.clt` probe binary, the other the 27-test suite for
`src/music.rs`. The probe came first, so `music` ran the probe and the suite
never ran; the harness reported "music" as a working mode for as long as it took
someone to read the label list by hand.

Usage:
    python3 tools/check-harness-arms.py [path/to/offline_harness.sh]

Exit status 0 when every label is reachable, 1 on the first duplicate (all
duplicates are listed). The label list is the same one `harness.sh arms` prints.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# `    music) ...` or `    matrix-maps) ...`. Anchored to four spaces so the
# indented `case "$name" in` arms inside `bins` are not mistaken for modes.
ARM = re.compile(r"^    ([a-z][a-z_|-]*)\)", re.MULTILINE)


def arms(text: str) -> list[tuple[str, int]]:
    """Every mode label in `text`, with its 1-based line number."""
    found: list[tuple[str, int]] = []
    for match in ARM.finditer(text):
        line = text.count("\n", 0, match.start()) + 1
        for label in match.group(1).split("|"):
            found.append((label, line))
    return found


def duplicates(found: list[tuple[str, int]]) -> list[tuple[str, list[int]]]:
    """Labels that appear more than once, with every line they appear on."""
    lines: dict[str, list[int]] = {}
    for label, line in found:
        lines.setdefault(label, []).append(line)
    return [(label, seen) for label, seen in lines.items() if len(seen) > 1]


def main(argv: list[str]) -> int:
    path = Path(argv[1] if len(argv) > 1 else "tools/offline_harness.sh")
    text = path.read_text(encoding="utf-8")
    found = arms(text)
    if not found:
        print(f"{path}: no mode labels found", file=sys.stderr)
        return 1
    repeats = duplicates(found)
    if repeats:
        for label, lines in repeats:
            listed = ", ".join(str(line) for line in lines)
            print(
                f"{path}: arm `{label}` is declared {len(lines)} times (lines {listed}); "
                "every one after the first is dead code",
                file=sys.stderr,
            )
        return 1
    print(f"{path}: {len(found)} mode labels, all reachable")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
