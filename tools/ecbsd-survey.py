#!/usr/bin/env python3
"""Survey EC-BSD placement files: version, name count, and whether names parse.

`src/grass.rs`, `src/water.rs` and `src/scene.rs` make three claims about these
files: the identity word, the revision they accept, and how many `BSData` names
the table can hold (`ECBSD_NAME_LIMIT`). This checks all three against real
files rather than against the reader.

    tools/ecbsd-survey.py <folder> [...]

A file that does not parse is printed as REFUSE with the reason -- never
skipped silently. The summary states the union the readers must accept.
"""

import collections
import glob
import os
import struct
import sys

IDENTITY = (0x42584F4D, 0x42534446)
HEADER = 0x4C


def path_string(b, p):
    """Mirror of `water::path_string`, including its three refusals."""
    if p + 4 > len(b):
        return None, p, "truncated name length"
    length = struct.unpack_from("<I", b, p)[0]
    p += 4
    if length > 1024:
        return None, p, f"name length {length} > 1024"
    if p + length > len(b):
        return None, p, "truncated name"
    raw = b[p:p + length]
    p += length
    try:
        text = raw.decode("gbk")
    except UnicodeDecodeError as error:
        return None, p, f"bad GBK: {error}"
    path = text.replace("\\", "/")
    if ".." in path or path.startswith("/") or ":" in path:
        return None, p, f"unsafe path {path!r}"
    return path, p, None


def survey(path):
    with open(path, "rb") as handle:
        b = handle.read()
    if len(b) < HEADER:
        return {"refuse": f"{len(b)} bytes, shorter than the {HEADER}-byte header"}
    identity = struct.unpack_from("<2I", b, 0)
    if identity != IDENTITY:
        return {"refuse": f"identity {identity[0]:#010x} {identity[1]:#010x}"}
    version, count = struct.unpack_from("<2I", b, 8)
    if count > len(b):
        return {"refuse": f"{count} names but only {len(b)} bytes"}
    names, p = [], HEADER
    for index in range(count):
        name, p, error = path_string(b, p)
        if error:
            return {"refuse": f"name {index} of {count}: {error}"}
        names.append(name)
    return {"version": version, "count": count, "names": names}


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    files = refused = 0
    versions, counts = collections.Counter(), []
    for root in argv[1:]:
        if not os.path.isdir(root):
            print(f"skip {root}: not a directory", file=sys.stderr)
            continue
        for path in sorted(glob.glob(os.path.join(root, "**", "*.ecbsd"), recursive=True)):
            files += 1
            name = os.path.basename(path)
            result = survey(path)
            if "refuse" in result:
                refused += 1
                print(f"REFUSE {name:20} {result['refuse']}")
                continue
            names = result["names"]
            print(
                f"ok     {name:20} v{result['version']:<3} names {result['count']:>4}  "
                f"first {names[0]!r}"
            )
            versions[result["version"]] += 1
            counts.append(result["count"])
    print()
    print(f"{files} file(s), {refused} refused")
    if counts:
        print(f"  revisions      : {dict(sorted(versions.items()))}")
        print(f"  name count     : min {min(counts)}, max {max(counts)}")
        print("  ECBSD_NAME_LIMIT headroom: "
              f"{10000 - max(counts)} names above the largest measured table")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
