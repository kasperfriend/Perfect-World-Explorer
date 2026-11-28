#!/usr/bin/env python3
"""Count the rows of the unsupported-path register, per table and in total.

Prose in `TODO.md` and the PR body quotes this number, and it had drifted: the
file says 62 while it holds 61. A count in a sentence is a claim about a file,
so it should be printed by a program rather than remembered.

Exits non-zero when the optional expected total does not match.
"""
import sys

PATH = "Findings/Unsupported-client-paths.md"
HEADERS = ("| Item", "| Path / behavior", "| ---")


def main(argv):
    section = None
    counts = {}
    order = []
    for line in open(PATH, encoding="utf-8"):
        if line.startswith("## "):
            section = line.strip("# \n")
            if section not in counts:
                counts[section] = 0
                order.append(section)
        elif (
            line.startswith("| ")
            and section
            and not line.startswith(HEADERS)
        ):
            counts[section] += 1
    total = sum(counts.values())
    for name in order:
        if counts[name]:
            print("%-70s %3d" % (name, counts[name]))
    print("%-70s %3d" % ("total", total))
    if len(argv) > 1 and argv[1].isdigit() and int(argv[1]) != total:
        print(
            "expected %s rows, found %d" % (argv[1], total),
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
