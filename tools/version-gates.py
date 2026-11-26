#!/usr/bin/env python3
"""Survey the version word of every file a version gate reads.

Every cap and every `!= N` in the readers is a claim about shipped data. This
script checks the claim against the files instead of trusting it: point it at a
client's `element` folder and it prints, per file type, every version word that
actually appears and how many files carry it.

    tools/version-gates.py <client element folder> [...]

It prints one block per client and a combined "all clients" block, so a limit
can be set from the union rather than from one install.

Formats surveyed here are the ones whose version word is a plain dword at a
known offset:

    .t2bk  terrain block        dword @4   (src/terrain_block.rs)
    .t2lm  terrain lightmap     dword @4   (src/terrain_lightmaps.rs)
    .trn2  terrain              dword @4   (src/terrain*.rs)
    .ecwld world                dword @4   (src/scene.rs)
    .ecbsd EC-BSD manifest      dword @8   (src/scene.rs)
    .clt   precinct/region      ASCII text header line (src/music.rs)
    .spt   SpeedTree            ASCII text header line
    .dat   BSData               magic+version dwords @0/@4
    elements.data              dword @0, read as (family << 16) | version
    tasks.data                  dword @4, after magic 0x93858361
    tasks_sample.data           same tasks header; aipolicy.data trigger @4

Other .data files are not treated as dword versions: several carry u16 tags,
timestamps, declared sizes or magic words at offset 0.

GFX and package versions are inside containers, so `offline_harness.sh gfx` and
`pck` report those where the real readers run.
"""

import collections
import os
import struct
import sys

# extension -> (label, offset of version dword, magic dword + expected value or None)
DWORD_FORMATS = {
    ".t2bk": ("terrain block", 4, (0, 0x54424B42)),
    ".t2lm": ("terrain lightmap", 4, None),
    ".t2lm1": ("terrain lightmap mip", 4, None),
    ".t2mk": ("terrain mask", 4, None),
    ".trn2": ("terrain", 4, (0, 0x54524E32)),
    ".ecwld": ("world", 4, (0, 0x45435744)),
    ".ecbsd": ("EC-BSD manifest", 8, (0, 0x42584F4D)),
    ".dat": ("BSData", 4, (0, 0x42584F4D)),
    # `elements.data` and `task_npc.data` put `(family << 16) | version` at 0,
    # which is what `designer::catalog::unsupported_layout` decodes. No magic to
    # check, so every `.data` that is not a BSData table lands here.
    ".data": ("elements/task data", 0, None),
}

TEXT_FORMATS = {
    ".clt": "precinct/region",
    ".spt": "SpeedTree",
}


def walk(root):
    for dirpath, _dirnames, filenames in os.walk(root):
        for name in filenames:
            yield os.path.join(dirpath, name)


def survey_client(root):
    found = collections.defaultdict(collections.Counter)
    bad_magic = collections.Counter()
    text_headers = collections.defaultdict(collections.Counter)
    files = 0
    for path in walk(root):
        ext = os.path.splitext(path)[1].lower()
        try:
            size = os.path.getsize(path)
        except OSError:
            continue
        if size < 16:
            continue
        try:
            with open(path, "rb") as handle:
                head = handle.read(64)
        except OSError:
            continue
        files += 1
        format_info = DWORD_FORMATS.get(ext)
        if ext == ".data":
            format_info = DATA_DWORD_FORMATS.get(os.path.basename(path).lower())
        if format_info is not None:
            label, offset, magic = format_info
            if size < offset + 4 or len(head) < offset + 4:
                continue
            if magic is not None:
                at, want = magic
                got = struct.unpack_from("<I", head, at)[0]
                if got != want:
                    bad_magic[(os.path.basename(path).lower(), hex(got))] += 1
                    continue
            version = struct.unpack_from("<I", head, offset)[0]
            found[label][version] += 1
        elif ext in TEXT_FORMATS:
            # These files open with an ASCII header such as `precinct` then a
            # version line; keep the first printable line as the version.
            line = head.split(b"\n", 1)[0]
            if line:
                text_headers[TEXT_FORMATS[ext]][
                    line.decode("latin-1").strip()[:48]
                ] += 1
    return files, found, bad_magic, text_headers


def render(label, counter, total):
    if not counter:
        return
    print(f"  {label}: {total} file(s)")
    for version, count in counter.most_common():
        if isinstance(version, int) and label.startswith("elements"):
            # The word is `(family << 16) | version`. Printing only the word
            # makes a newer client's v70 look like an unknown format instead of
            # a newer revision of the family the reader already handles, which
            # is the exact mistake `unsupported_layout` was written to avoid.
            print(
                f"    family 0x{version >> 16:04x}  version {version & 0xffff:<6} "
                f"header 0x{version:08x}  x{count}"
            )
        elif isinstance(version, int):
            print(f"    version {version:<6} 0x{version:08x}  x{count}")
        else:
            print(f"    {version!r}  x{count}")


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    combined = collections.defaultdict(collections.Counter)
    combined_text = collections.defaultdict(collections.Counter)
    for root in argv[1:]:
        if not os.path.isdir(root):
            print(f"skip {root}: not a directory", file=sys.stderr)
            continue
        print(f"== {root}")
        files, found, bad_magic, text = survey_client(root)
        print(f"  {files} file(s) examined")
        for label, counter in sorted(found.items()):
            render(label, counter, sum(counter.values()))
        for label, counter in sorted(text.items()):
            render(label, counter, sum(counter.values()))
        for (ext, got), count in sorted(bad_magic.items()):
            print(f"  {ext}: {count} file(s) with magic {got} (wrong format, not surveyed)")
        for label, counter in found.items():
            combined[label].update(counter)
        for label, counter in text.items():
            combined_text[label].update(counter)
    print("== all clients")
    for label, counter in sorted(combined.items()):
        render(label, counter, sum(counter.values()))
    for label, counter in sorted(combined_text.items()):
        render(label, counter, sum(counter.values()))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
