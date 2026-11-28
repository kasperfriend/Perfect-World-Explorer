#!/usr/bin/env python3
"""Two silent-failure checks on `tools/offline_harness.sh`.

**Dead `case` arms.**

A shell `case` takes its first match. Two arms carrying the same label is not a
syntax error and it prints nothing at all: the second arm simply never runs, and
every mode that was supposed to reach it silently does something else. That is
not hypothetical. This script exists because two arms were labelled `music` —
one was a `precinct.clt` probe binary, the other the 27-test suite for
`src/music.rs`. The probe came first, so `music` ran the probe and the suite
never ran; the harness reported "music" as a working mode for as long as it took
someone to read the label list by hand.

**Unescaped backticks in a heredoc body.** The harness writes its generated Rust
through unquoted heredocs, so `$REPO` and friends expand — and so does every
bare backtick pair. A comment containing `` `terrain_block` `` therefore *ran*
`terrain_block` while writing the file: the shell printed
`terrain_block: command not found`, and the words between the backticks were
replaced by empty output in the generated source. Both times it happened the
only symptom was one stray line on stderr during an unrelated mode. Backticks in
a heredoc body must be written `` \` ``.

Usage:
    python3 tools/check-harness.py [path/to/offline_harness.sh]

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


def heredoc_bodies(text: str) -> list[tuple[int, str]]:
    """Lines inside an unquoted heredoc body, with 1-based line numbers.

    Quoted heredocs (`<<'EOF'`) do not expand anything, so a backtick there is
    harmless and is not reported.
    """
    body: list[tuple[int, str]] = []
    inside = False
    for number, line in enumerate(text.split("\n"), start=1):
        stripped = line.strip()
        if not inside:
            # `cat > "$OUT/x" <<EOF` — unquoted delimiter, expansions live.
            match = re.search(r"<<\s*([A-Za-z_][A-Za-z0-9_]*)\s*$", line)
            if match and "cat" in line:
                inside = True
            continue
        if stripped == "EOF" or stripped == match.group(1):
            inside = False
            continue
        body.append((number, line))
    return body


def stray_backticks(text: str) -> list[tuple[int, str]]:
    """Heredoc-body lines carrying an unescaped backtick."""
    bad = []
    for number, line in heredoc_bodies(text):
        for index, ch in enumerate(line):
            if ch == "`" and (index == 0 or line[index - 1] != "\\"):
                bad.append((number, line.strip()))
                break
    return bad


def main(argv: list[str]) -> int:
    path = Path(argv[1] if len(argv) > 1 else "tools/offline_harness.sh")
    text = path.read_text(encoding="utf-8")
    status = 0
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
        status = 1
    else:
        print(f"{path}: {len(found)} mode labels, all reachable")
    strays = stray_backticks(text)
    if strays:
        for number, line in strays:
            print(
                f"{path}:{number}: unescaped backtick in a heredoc body -- the shell will run "
                f"whatever is between the backticks and write its output instead: {line}",
                file=sys.stderr,
            )
        status = 1
    else:
        print(f"{path}: no unescaped backticks in heredoc bodies")
    return status


if __name__ == "__main__":
    sys.exit(main(sys.argv))
