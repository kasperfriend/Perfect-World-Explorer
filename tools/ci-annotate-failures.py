#!/usr/bin/env python3
"""Republish the useful part of a failing CI log as GitHub annotations.

Annotations are the only part of a failing job that can be read back through the
API. A step that simply dies leaves an agent with "conclusion: failure" and no
reason, which has already cost several CI cycles on this branch. This script is
run by ``tools/ci-run-and-annotate.sh`` when the wrapped command fails.

Sampling the head and tail of the log does not work: the head is checkout advice
and the tail is the last few passing tests. So this looks for the lines that
actually explain a failure -- rustc's own errors, the names of failing tests, and
any panic message -- and annotates those with a few lines of context.

It never changes an exit status; the caller re-raises the wrapped command's own.
"""

import re
import sys

# Ordered roughly by how directly each one names a cause.
PATTERNS = [
    r"^error(\[E\d+\])?:",
    r"^error: could not compile",
    r"^error: test failed",
    r"test .* FAILED",
    r"^failures:$",
    r"^---- .* stdout ----",
    r"panicked at",
    r"^thread .* panicked",
    r"assertion .* failed",
    r"^\s*left: ",
    r"^\s*right: ",
]

# rustc puts the source line and the caret on the lines after the error.
CONTEXT = 4
LIMIT = 24
CHUNK = 380


def annotations(lines):
    """Return one short string per failure line found, with context."""
    found = []
    # A failure usually matches several patterns on consecutive lines; emitting a
    # window per match repeats the same four lines over and over. Skip any match
    # that a window already covered.
    covered_until = 0
    for index, line in enumerate(lines):
        if index < covered_until:
            continue
        if not any(re.search(pattern, line) for pattern in PATTERNS):
            continue
        block = lines[index : index + CONTEXT]
        text = " | ".join(part.strip() for part in block if part.strip())
        found.append(text[:CHUNK])
        covered_until = index + CONTEXT
        if len(found) >= LIMIT:
            break
    if found:
        return found
    # Say so, rather than printing nothing and leaving the failure unexplained.
    tail = [line[:CHUNK] for line in lines[-6:]]
    return ["no line matched a known failure pattern; last lines follow"] + tail


def main(argv):
    if len(argv) != 2:
        print("usage: ci-annotate-failures.py <log>", file=sys.stderr)
        return 2
    with open(argv[1], errors="replace") as handle:
        lines = handle.read().splitlines()
    for number, text in enumerate(annotations(lines)):
        # These two characters terminate an annotation early, so escape them.
        safe = text.replace("%", "%25").replace("\r", "%0D")
        print(f"::error title=harness failure {number}::{safe}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
