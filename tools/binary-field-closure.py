"""Name struct literals in a `src/bin` target that are missing a declared field.

Adding a field to a shared struct is a two-line change in `src/` and a latent
compile error in every binary that builds the struct by hand. `cargo check
--bins` finds it, but only one target at a time and only in CI; the offline
harness skips every binary that mentions `wgpu`, which is where most of the
hand-built fixtures live. `Trees::off_reason` cost exactly that: two GPU
targets stopped compiling and nothing here could see it.

This is a text check, not a type check. It follows the `#[path]` graph from one
binary, indexes every `pub struct` the files it reaches declare, then reports a
literal of one of those structs whose top-level fields do not include every
declared field. A literal with a `..` spread is skipped, because it is allowed
to be short. Output is one `file:line Struct missing a, b` per finding; exit
status is 1 only when a file cannot be read.

    python3 tools/binary-field-closure.py src/bin/tree-lod-gpu-check.rs
"""

import os
import re
import sys

SRC = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src")
PATH_MOD = re.compile(r'#\[path\s*=\s*"([^"]+)"\]\s*(?:pub\s+)?mod\s+\w+\s*;')
# A string literal can contain braces -- `"Page {pages}: ..."` is a format
# string, not a struct literal -- and a `//` comment can hold anything. Both are
# blanked before the literal scan, keeping the byte count so line numbers stay
# right. A raw string (`r#"..."#`) is left alone: none of these binaries use one
# in a position that would matter, and guessing at it would be worse.
STRING = re.compile(r'"(?:\\.|[^"\\])*"')
LINE_COMMENT = re.compile(r"//[^\n]*")


def balanced(text: str, open_at: int) -> str:
    """The body of the brace that starts at `open_at`, without the braces."""
    depth = 0
    for index in range(open_at, len(text)):
        if text[index] == "{":
            depth += 1
        elif text[index] == "}":
            depth -= 1
            if depth == 0:
                return text[open_at + 1 : index]
    return text[open_at + 1 :]


def split_top_level(body: str) -> list:
    """Split on commas that are not inside braces, brackets or parens.

    Only real brackets count. `<` and `>` are deliberately not tracked: a
    comparison (`a < b`) would open a level that never closes, and a turbofish
    would close one that was never opened. Angle brackets cannot hide a
    top-level comma in the field shapes this file looks at.
    """
    parts, depth, current = [], 0, ""
    for char in body:
        if char in "{([":
            depth += 1
        elif char in "})]":
            depth -= 1
        if char == "," and depth == 0:
            parts.append(current)
            current = ""
        else:
            current += char
    parts.append(current)
    return parts


def declared_structs(paths) -> dict:
    """`file -> {struct name -> [field, ...]}` for every `pub struct` with fields."""
    found = {}
    for path in paths:
        try:
            text = open(path, encoding="utf-8", errors="replace").read()
        except OSError:
            continue
        for match in re.finditer(r"pub struct (\w+)\s*\{", text):
            # Comments go first: a doc comment above a field can contain a comma
            # ("no cache, no DLL"), which would split the field list inside the
            # comment and leave the real field merged with it.
            body = LINE_COMMENT.sub(
                lambda m: " " * len(m.group(0)), balanced(text, match.end() - 1)
            )
            fields = []
            for part in split_top_level(body):
                part = part.strip()
                if not part or part.startswith("//") or part.startswith("#"):
                    continue
                name = re.match(r"(?:pub(?:\s*\([^)]*\))?\s+)?(\w+)\s*:", part, re.S)
                if name:
                    fields.append(name.group(1))
            if fields:
                found.setdefault(os.path.normpath(path), {})[match.group(1)] = fields
    return found


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
    # Every file this binary reaches, plus which module each one is declared
    # as, so `skin::Vertex` and `model_draw::Vertex` stay distinct: they have
    # the same name and different fields, and only one of them is being built.
    reached = [os.path.normpath(target)]
    code = LINE_COMMENT.sub(lambda m: " " * len(m.group(0)), text)
    code = STRING.sub(lambda m: " " * len(m.group(0)), code)
    module_of = {}
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
        reached.append(full)
        queue.extend(PATH_MOD.findall(open(full, encoding="utf-8", errors="replace").read()))
    for relative, name in re.findall(
        r'#\[path\s*=\s*"([^"]+)"\]\s*(?:pub\s+)?mod\s+(\w+)\s*;', text
    ):
        module_of[name] = os.path.normpath(os.path.join(os.path.dirname(target), relative))
    structs = declared_structs(reached)
    # A bare `Name {` resolves only when exactly one reached file declares it; an
    # ambiguous name is skipped rather than guessed at.
    by_name = {}
    for path, table in structs.items():
        for name, fields in table.items():
            by_name.setdefault(name, []).append((path, fields))
    found = []
    for match in re.finditer(r"\b(?:(\w+)::)?([A-Z]\w*)\s*\{", code):
        module, name = match.group(1), match.group(2)
        if module is not None:
            path = module_of.get(module)
            fields = structs.get(path, {}).get(name) if path else None
        else:
            candidates = by_name.get(name, [])
            fields = candidates[0][1] if len(candidates) == 1 else None
        if not fields:
            continue
        # `impl X {`, `struct X {` and a match arm (`::Page(p) =>`) are not
        # literals: the first two open an item, the third is a pattern whose
        # "fields" are bindings.
        lead = code[: match.start()].rstrip()
        if lead.endswith(("struct", "impl", "enum", "for", "trait", "(", "=>")):
            continue
        body = balanced(code, match.end() - 1)
        if re.search(r"\.\.[\w:]", body):
            continue
        present = set()
        for part in split_top_level(body):
            field = re.match(r"\s*(\w+)\s*(?::|$)", part, re.S)
            if field:
                present.add(field.group(1))
        if not present:
            continue
        missing = [field for field in fields if field not in present]
        if missing:
            line = code[: match.start()].count("\n") + 1
            found.append(f"{target}:{line} {name} missing {', '.join(missing)}")
    for line in found:
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main())
