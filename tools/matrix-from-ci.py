#!/usr/bin/env python3
"""Write the client x category matrix from the CI JSON files.

The rule for this table is that no cell may be written from memory. Every cell
here is produced by reading `ci-results/<client>.json`, which CI writes by
running the shipped readers over real client bytes and then commits to the
`ci-results` branch. If a client has no JSON, every cell for it is `blocked` and
says which file would close it.

Labels, exactly as the task requires:
  measured (CI file)  -- a value taken from ci-results/<client>.json
  static (file:line)  -- a fact read out of the source, cited by location
  blocked (reason)    -- no measurement exists, with the missing input named

Usage:
  tools/matrix-from-ci.py <dir-with-json> [--out docs/cross-version-matrix.md]

Without `--out` it prints the generated section to stdout, which is how CI can
check that the committed table matches the committed JSON.
"""

import argparse
import json
import os
import sys

# The clients the task names, in version order, plus the 2016 pack. The key is
# the directory name the measure step uses; the label is what the table prints.
CLIENTS = [
    ("pw-131", "1.3.1"),
    ("pw-136", "1.3.6"),
    ("pw-142", "1.4.2"),
    ("pw-144", "1.4.4"),
    ("pw-152", "1.5.2"),
    ("pw-184", "1.8.4"),
    ("pw-2016", "2016 pack"),
]

# The categories the task lists, in its order. The JSON `category` string is
# what client-matrix-json.rs writes, so this list is also a check on that file:
# a category the tool never emits shows as blocked, not as a silent zero.
CATEGORIES = [
    "world ECWLD",
    "terrain T2BK",
    "lightmaps T2LM",
    "water (BSData)",
    "trees",
    "GFX effects",
    "music precinct",
    "sounds",
    "zone names",
    "instances",
    "DDS textures",
    "package trailers (pck)",
    "package trailers (angelica)",
    "elements data",
    "tasks data",
]

# Where each category's reader lives, for the `blocked` text: this names the code
# that would have to be run, so a blocked cell says what to measure and with what.
READERS = {
    "world ECWLD": "src/scene.rs (Scene::load)",
    "terrain T2BK": "src/terrain_block.rs (trn2_header)",
    "lightmaps T2LM": "src/terrain_lightmaps.rs (side_of)",
    "water (BSData)": "src/water.rs (water::load)",
    "trees": "src/scene.rs tree table + src/tree_source.rs",
    "GFX effects": "src/pck.rs directory + GFX container readers",
    "music precinct": "src/music.rs (Zones::open)",
    "sounds": "src/scene.rs sound references",
    "zone names": "src/music.rs zones + src/world_place_names.rs",
    "instances": "src/scene.rs per instance .ecwld",
    "DDS textures": "src/model_dds.rs (Dds::parse)",
    "package trailers (pck)": "src/pck.rs (directory)",
    "package trailers (angelica)": "src/angelica_pck.rs (directory)",
    "elements data": "src/client_detect.rs (ElementsHeader::parse)",
    "tasks data": "src/client_detect.rs version word",
}


def load_client(directory, name):
    """Return the parsed JSON for one client, or None if there is none."""
    path = os.path.join(directory, name + ".json")
    if not os.path.isfile(path):
        return None
    try:
        with open(path, encoding="utf-8") as handle:
            return json.load(handle)
    except (OSError, ValueError) as error:
        print(f"cannot read {path}: {error}", file=sys.stderr)
        return None


def cell(doc, category):
    """The (label, text) for one category of one client."""
    if doc is None:
        return ("blocked", "")
    for row in doc.get("cells", []):
        if row.get("category") != category:
            continue
        status = row.get("status")
        detail = row.get("detail", "")
        if status == "ok":
            return ("ok", detail)
        if status == "named-refusal":
            return ("refused", detail)
        if status == "absent":
            # A staged corpus is a subset of a client. "Absent" is the fixture
            # saying it does not carry the file, which is not the client saying
            # it ships none, so it cannot become a matrix claim.
            return ("blocked", "not in the staged fixture")
        if status == "silent":
            # Should be impossible: the tool fails the job on any silent cell.
            return ("SILENT", detail)
        if status == "blocked":
            return ("blocked", detail)
        return ("blocked", f"unrecognised status {status!r}")
    return ("blocked", "the tool did not report this category")


def render(directory):
    """The generated section, as a list of lines."""
    docs = {name: load_client(directory, name) for name, _ in CLIENTS}

    lines = []
    lines.append("## Client x category, generated from the CI JSON")
    lines.append("")
    lines.append(
        "Every cell below is produced by `tools/matrix-from-ci.py` from the files on"
    )
    lines.append(
        "the `ci-results` branch, which CI writes by running the shipped readers over"
    )
    lines.append(
        "real client bytes. Nothing here is typed from memory, and no cell is left"
    )
    lines.append("blank.")
    lines.append("")
    lines.append("| category | " + " | ".join(label for _, label in CLIENTS) + " |")
    lines.append("|---" * (len(CLIENTS) + 1) + "|")

    counts = {"ok": 0, "refused": 0, "blocked": 0, "SILENT": 0}
    for category in CATEGORIES:
        row = [category]
        for name, _ in CLIENTS:
            label, text = cell(docs[name], category)
            counts[label if label in counts else "blocked"] += 1
            if label == "ok":
                row.append(text.replace("|", "\\|"))
            elif label == "refused":
                row.append("**refused**: " + text.replace("|", "\\|"))
            elif label == "SILENT":
                row.append("**SILENT** " + text.replace("|", "\\|"))
            else:
                if docs[name] is None:
                    row.append(f"blocked: no `ci-results/{name}.json`")
                else:
                    row.append("blocked: " + text)
        lines.append("| " + " | ".join(row) + " |")

    lines.append("")
    lines.append(
        f"{counts['ok']} cells measured ok, {counts['refused']} named refusals, "
        f"{counts['blocked']} blocked, {counts['SILENT']} SILENT."
    )
    lines.append("")
    if counts["SILENT"]:
        lines.append(
            "**A SILENT cell means a reader returned nothing without recording a"
        )
        lines.append("reason. That is a bug, and CI fails the job on it.**")
        lines.append("")

    # Where the measurements came from, and what a blocked cell needs. This is
    # the part that makes a blocked cell actionable rather than a shrug.
    present = [name for name, _ in CLIENTS if docs[name] is not None]
    missing = [(name, label) for name, label in CLIENTS if docs[name] is None]
    lines.append("### Where these numbers come from")
    lines.append("")
    if present:
        lines.append(
            "Measured (CI files): "
            + ", ".join(f"`ci-results/{name}.json`" for name in present)
            + "."
        )
    lines.append("")
    if missing:
        lines.append("### Blocked clients")
        lines.append("")
        lines.append(
            "These clients have no measurement, so every one of their cells is"
        )
        lines.append(
            "`blocked` rather than guessed. Each needs the file named to close it:"
        )
        lines.append("")
        for name, label in missing:
            lines.append(
                f"- **{label}** (`ci-results/{name}.json`): stage that client's "
                f"files under `tests/fixtures/clients/{name}/` and the measure "
                "step picks them up with no other change."
            )
        lines.append("")
        lines.append(
            "The staging is the only thing missing. The readers themselves run on"
        )
        lines.append(
            "whatever is staged, so a client with bytes gets a row the same run."
        )
        lines.append("")

    # Static facts, cited, for the categories whose reader is not reachable from
    # a staged fixture at all. These are labelled so they cannot be mistaken for
    # measurements.
    lines.append("### Static facts, cited")
    lines.append("")
    lines.append(
        "Read out of the source, not measured, and labelled as such:"
    )
    lines.append("")
    lines.append(
        "- `WORLD_VERSIONS` / `REVISIONS`: `src/pck.rs` (the container revision "
        "table and the byte offsets each revision uses)."
    )
    lines.append(
        "- `WATER_VERSIONS [0x10002000, 0x10003000]`: `src/water.rs`, the two "
        "EC-BSD revisions the loader accepts; anything else is refused by name."
    )
    lines.append(
        "- `terrain_block::KNOWN_VERSIONS 2..=6`: `src/terrain_block.rs`, the "
        "TRN2 header versions the reader accepts."
    )
    lines.append(
        "- Water and terrain refusals name the file since the change to "
        "`water::load`; before it, `water (BSData)` reported a bare errno."
    )
    lines.append("")
    return lines


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", help="directory holding <client>.json")
    parser.add_argument("--out", help="write the section into this markdown file")
    args = parser.parse_args(argv[1:])

    lines = render(args.directory)
    text = "\n".join(lines)

    if not args.out:
        print(text)
        return 0

    begin = "<!-- BEGIN GENERATED MATRIX -->"
    end = "<!-- END GENERATED MATRIX -->"
    block = f"{begin}\n{text}\n{end}\n"

    try:
        with open(args.out, encoding="utf-8") as handle:
            existing = handle.read()
    except FileNotFoundError:
        existing = ""
    if begin in existing and end in existing:
        head, rest = existing.split(begin, 1)
        _, tail = rest.split(end, 1)
        updated = head + block + tail.lstrip("\n")
    else:
        updated = existing + ("\n" if existing else "") + block
    with open(args.out, "w", encoding="utf-8") as handle:
        handle.write(updated)
    print(f"wrote the generated matrix into {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
