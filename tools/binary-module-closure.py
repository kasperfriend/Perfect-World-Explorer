"""Name every `src/*.rs` module a `src/bin` target reaches but does not declare.

A binary under `src/bin` is its own crate root: it declares the modules it
needs with `#[path = "../x.rs"] mod x;`, and any `crate::y` inside one of those
files has to be declared too, or the binary does not compile. `cargo check
--bins` catches that in CI, but the offline harness skips every binary that
mentions `wgpu`, so a missing declaration in a GPU target is invisible here --
that is exactly how `model-draw-gpu-check` lost `effect_census`.

This follows the `#[path]` graph from one binary, collects `crate::name`
references over the files it reaches, and prints the names that resolve to an
existing `src/name.rs` but are not declared. Exit status 1 means the file could
not be read; an empty stdout with status 0 means the closure is complete.

    python3 tools/binary-module-closure.py src/bin/model-draw-gpu-check.rs
"""

import os
import re
import sys

SRC = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src")

DECLARED = re.compile(r"^\s*(?:pub\s+)?mod\s+(\w+)\s*[;{]", re.M)
PATH_MOD = re.compile(r'#\[path\s*=\s*"([^"]+)"\]\s*(?:pub\s+)?mod\s+\w+\s*;')
# A reference is either `crate::name` or a bare `name` inside a
# `use crate::{a, b::{c}, d as e};` group. Documentation links
# (`[`crate::x`]`, `\`crate::x::y\``) are not references: they cost nothing to
# resolve and name modules the file never uses, so they are stripped first.
CRATE_REF = re.compile(r"crate::(\w+)")
USE_GROUP = re.compile(r"use\s+crate::\s*\{(.*?)\}\s*;", re.S)
DOC_LINK = re.compile(r"\[`[^`]*`\]|`[^`\n]*`")


def strip_documentation(body: str) -> str:
    """Drop comment lines and inline doc links, keeping code.

    A reference in a doc comment is not a dependency: `[`crate::sky_time`]` in a
    module header names a module the file does not use, and counting it would
    report a missing declaration for a target that compiles. Whole-line `//`
    and `///` lines go, and so does any `` `code` `` span that is still left, so
    a trailing `` (see `crate::x`) `` comment does not count either.
    """
    kept = []
    for line in body.split("\n"):
        stripped = line.lstrip()
        if stripped.startswith("//") or stripped.startswith("/*") or stripped.startswith("*"):
            continue
        kept.append(DOC_LINK.sub("", line))
    return "\n".join(kept)


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    target = sys.argv[1]
    try:
        text = open(target, encoding="utf-8", errors="replace").read()
    except OSError as error:
        print(error, file=sys.stderr)
        return 1
    declared = set(DECLARED.findall(text))
    references = set()
    seen = set()
    queue = list(PATH_MOD.findall(text))
    while queue:
        relative = queue.pop()
        if relative in seen:
            continue
        seen.add(relative)
        full = os.path.normpath(os.path.join(os.path.dirname(target), relative))
        if not os.path.exists(full):
            continue
        body = strip_documentation(open(full, encoding="utf-8", errors="replace").read())
        references.update(CRATE_REF.findall(body))
        for group in USE_GROUP.findall(body):
            # Only the outermost item of each entry: `a`, `b::{c}` and `d as e`
            # all name `a`, `b` and `d`.
            for entry in group.split(","):
                name = entry.strip().split("::")[0].split(" as ")[0].strip()
                if re.fullmatch(r"\w+", name):
                    references.add(name)
        # A file that itself carries `#[path]` modules (a fixture, or a module
        # that pulls in its own dependency) is followed as well.
        queue.extend(PATH_MOD.findall(body))
    shipped = {
        name[: -len(".rs")]
        for name in os.listdir(SRC)
        if name.endswith(".rs")
    }
    missing = sorted(references & shipped - declared)
    if missing:
        print(", ".join(missing))
    return 0


if __name__ == "__main__":
    sys.exit(main())
