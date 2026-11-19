#!/usr/bin/env python3
"""Generate the instance zone-name aliases from a client's `instance.txt`.

`configs/instance.txt` is inside `configs.pck`: UTF-16, a quoted Chinese display
name, `{`, the numeric instance id, `//  ID`, and the folder path (`"a07"`), for
every instance map the client ships. The zone banner in an instance reads its
name from that map's own `precinct.clt`, which is Chinese in a Chinese client, so
the banner shows boxes unless the name is localised.

Two tables answer that question:

* `src/world_place_names.rs` — world zone names, built by polygon matching
  between the Chinese and English `precinct.clt` for the same map.
* `src/world_catalog.rs::english_name` — instance labels by folder id, which is
  what the map picker already shows.

This script joins the two into the alias block in `src/world_place_names.rs`, so
that a zone-name lookup in an instance map answers with the English instance name
without anyone typing a character of Chinese by hand.

Usage:
    python3 tools/instance-aliases.py CONFIGS_PCK [--write]

Without `--write` it prints the block it would insert and the ids it could not
localise. With `--write` it replaces the text between the BEGIN and END markers.

The English side comes from the checked-in `english_name` table, read out of the
source, so this needs no client but the one whose `instance.txt` is being added.
"""

import argparse
import pathlib
import re
import sys

BEGIN = "// BEGIN GENERATED instance aliases"
END = "// END GENERATED instance aliases"

WORLD_PLACE_NAMES = pathlib.Path("src/world_place_names.rs")
WORLD_CATALOG = pathlib.Path("src/world_catalog.rs")


def read_utf16(path):
    raw = pathlib.Path(path).read_bytes()
    # `configs.pck` members are stored zlib-compressed; accept either a bare
    # `instance.txt` or a pack and pull the member out with the survey helper.
    if raw[:2] == b"\xff\xfe" or (len(raw) > 4 and raw[1] == 0):
        return raw.decode("utf-16")
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
    import pck_lib

    _, records = pck_lib.directory(path)
    for entry in records:
        if entry[0].lower() == "configs/instance.txt":
            return pck_lib.read_entry(path, entry).decode("utf-16")
    raise SystemExit(f"{path}: no configs/instance.txt member")


def instances(text):
    """(id, folder) pairs in file order, duplicates kept."""
    text = text.replace("\r", "")
    pattern = re.compile(r'"([^"]+)"\s*\{\s*(\d+)\s*//[^\n]*\n"([^"]+)"')
    return [(name, folder) for name, _, folder in pattern.findall(text)]


def english_table():
    source = WORLD_CATALOG.read_text()
    body = source.split("pub fn english_name", 1)[1]
    return dict(re.findall(r'"([a-z0-9]+)" => "([^"]+)"', body))


def block(pairs):
    lines = [BEGIN + " -- tools/instance-aliases.py"]
    lines.append("/// `(Chinese instance name, the English label for that folder)`.")
    lines.append("///")
    lines.append("/// Generated from the client's own `configs/instance.txt` joined with")
    lines.append("/// `world_catalog::english_name`, so the Chinese is the client's text and the")
    lines.append("/// English is the table the map picker already shows. Regenerate with")
    lines.append("/// `python3 tools/instance-aliases.py <configs.pck> --write`.")
    lines.append("pub const INSTANCE_NAMES: &[(&str, &str)] = &[")
    for chinese, english in pairs:
        lines.append(f'    ("{chinese}", "{english}"),')
    lines.append("];")
    lines.append(END)
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("pack", help="configs.pck (or a bare instance.txt)")
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()

    found = instances(read_utf16(args.pack))
    english = english_table()
    pairs = []
    missing = []
    seen = set()
    for chinese, folder in found:
        label = english.get(folder.lower())
        if label is None:
            if chinese not in missing:
                missing.append(f"{folder}\\t{chinese}")
            continue
        # The same Chinese name can name several folders (six "城战" rows all
        # point at b30); one alias per name is enough, and a duplicate would
        # make the lookup depend on row order.
        if chinese in seen:
            continue
        seen.add(chinese)
        pairs.append((chinese, label))
    pairs.sort(key=lambda row: row[0])

    print(f"{len(found)} instance rows, {len(pairs)} localised, {len(missing)} not in english_name")
    for row in missing:
        print(f"  no English label: {row}")
    text = block(pairs)
    if not args.write:
        print(text)
        return 0
    source = WORLD_PLACE_NAMES.read_text()
    start = source.find(BEGIN)
    end = source.find(END)
    if start < 0 or end < 0:
        raise SystemExit(f"{WORLD_PLACE_NAMES}: the BEGIN/END markers are missing")
    source = source[:start] + text + source[end + len(END):]
    WORLD_PLACE_NAMES.write_text(source)
    print(f"wrote {WORLD_PLACE_NAMES}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
