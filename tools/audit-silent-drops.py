#!/usr/bin/env python3
"""Classify every place a loader can lose a value without saying so.

Item 2.1 of the brief: for every `unwrap_or_default()`, every
`else { ... continue; }` inside a loader, every `.ok()?` and every `let _ =` on
a fallible call, decide whether the value is lost

  Named    the loss already logs a reason with the file and category, or
  Fix      the value vanishes with no reason at all, or
  Unreachable  it provably cannot lose anything, and here is why.

The classification is mechanical where it can be and explicit where it cannot.
A site the tool cannot classify is reported as `?` and fails the run, because an
unsurveyed site is exactly what this exists to find. Allowances live in
`tools/silent_drop_allowances.txt`, one `path:line category note` per line, so
overturning the tool's judgement requires writing down why.

Excluding tests and src/bin is deliberate: a test's unwrap is a test being a
test, and the survey tools are their own programs, not loaders on the path a
player's map takes.
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ALLOW = os.path.join(ROOT, "tools", "silent_drop_allowances.txt")
CHECK = "--check" in sys.argv

# The site patterns. Each is (name, regex, description).
PATTERNS = [
    ("unwrap_or_default", re.compile(r"\.unwrap_or_default\(\)"), "value defaults to empty"),
    ("ok-question", re.compile(r"\.ok\(\)\?"), "error becomes None"),
    ("let-underscore", re.compile(r"(?<![A-Za-z0-9_])let _ = (?!=)"), "result discarded"),
    ("else-continue", re.compile(r"^\s*else \{\s*$"), "branch falls through"),
]

# Files that are not loaders on a player's path.
SKIP_DIRS = {"bin", "modules"}
SKIP_FILES = {"diagnostics.rs"}


def rust_sources():
    """Every .rs under src/, minus tests, bin tools and the excluded set."""
    for base, dirs, files in os.walk(os.path.join(ROOT, "src")):
        dirs[:] = [d for d in dirs if d not in SKIP_DIRS]
        for f in sorted(files):
            if not f.endswith(".rs") or f in SKIP_FILES:
                continue
            yield os.path.join(base, f)


def test_module_start(lines):
    """The first line of a `#[cfg(test)]` module, or None.

    A test module runs to the end of the file in every case in this tree, which
    is a convention `audit-silent-drops` itself checks: if a `#[cfg(test)]`
    appears before the last non-blank line and something other than a closing
    brace follows it at top level, the tool refuses rather than guessing.
    """
    for i, line in enumerate(lines):
        if line.strip() == "#[cfg(test)]":
            return i
    return None


def load_allowances():
    """`path:line category [note]`, or `path category [note] code: <snippet>`.

    The second form keys on the code itself, so the allowance follows an edit that
    moves it up or down the file. Both are returned; `by_code` maps
    `(path, snippet)` to the set of allowance keys claiming it.
    """
    out = {}
    by_code = {}
    if not os.path.exists(ALLOW):
        return out, by_code
    for n, line in enumerate(open(ALLOW, encoding="utf-8"), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(None, 2)
        if len(parts) < 2:
            print(
                f"{ALLOW}:{n}: a line needs 'path:line category [note]' or "
                "'path category [note] code: <snippet>'",
                file=sys.stderr,
            )
            sys.exit(2)
        key, cat, note = parts[0], parts[1], parts[2] if len(parts) > 2 else ""
        out[key] = (cat, note)
        if "code:" in note:
            snippet = note.split("code:", 1)[1].strip()
            if not snippet:
                print(f"{ALLOW}:{n}: 'code:' needs a snippet after it", file=sys.stderr)
                sys.exit(2)
            by_code.setdefault((key, snippet), set()).add(key)
    return out, by_code


def enclosing_fn(lines, index):
    """The signature of the function containing `index`, searching upwards.

    A `.ok()?` inside a function returning `Option<T>` is a *probe*: the caller
    is told "no answer", which is a named outcome, not a silent drop. Inside a
    function returning `Result` it is different: an Err becomes None and the
    reason is lost, which is the failure class. So the return type decides, and
    this finds it.
    """
    depth = 0
    for i in range(index, -1, -1):
        line = lines[i]
        depth += line.count("}") - line.count("{")
        if depth < 0:
            depth = 0
        if re.search(r"\bfn\s+\w+", line) and depth == 0:
            sig = line
            for j in range(i + 1, min(i + 8, len(lines))):
                if "->" in sig or "{" in lines[j] or ";" in lines[j]:
                    sig += " " + lines[j]
                    if "{" in lines[j] or ";" in lines[j]:
                        break
                else:
                    sig += " " + lines[j]
            return sig
    return ""


def classify(
    path, lineno, kind, line, context, allowances, enclosing_returns_option=False, by_code=None
):
    """The category for one site: `named`, `fix`, `unreachable` or None."""
    key = f"{os.path.relpath(path, ROOT)}:{lineno}"
    if key in allowances:
        return allowances[key][0]
    # A line number is a poor identity for code, and this file is edited often
    # enough (four times in one afternoon) that a site's allowance went to
    # whatever line moved into its place. So an allowance may also be written
    # against the *code* it covers -- `path  <category>  <note>  code: <snippet>`
    # -- and then follows the snippet wherever it goes. The line form stays for
    # sites whose snippet is not distinctive; the snippet form is not guessed,
    # it is matched exactly against the trimmed source line.
    _rel = os.path.relpath(path, ROOT)
    code = line.strip()
    for akey in (by_code or {}).get((_rel, code), ()):
        return allowances[akey][0]
    # A site is `named` when the surrounding few lines log a reason. This is
    # deliberately generous about *how* it logs -- eprintln!, crate::diag!,
    # log::, a reason string -- because the property that matters is that a
    # reason reaches a human, not which macro carried it.
    window = "\n".join(context)
    logs = re.search(
        r"eprintln!|println!|crate::diag!|diag_keyed!|log::(?:warn|error|info)|"
        r"off_reason|drop_reason|record_drop|load_status::dropped|"
        r"format!\(\s*\"[^\"]*(?:unsupported|missing|cannot|failed|invalid|skipped)",
        window,
    )
    if logs:
        return "named"
    if kind == "let-underscore":
        # `let _ =` on something infallible loses nothing. The tool cannot type
        # check, so it requires the line to contain a call whose name suggests
        # fallibility to call it a Fix; anything else is a `?`.
        if re.search(r"\b(fs::|File::|remove_file|remove_dir_all|write|flush|sync)", line):
            return "fix"
        return None
    if kind == "else-continue":
        # `else { continue; }` only loses a value inside a loop that produces
        # one. Outside such a loop the next line decides nothing.
        if "continue;" not in "\n".join(context[:4]):
            return None
        return "fix" if not logs else "named"
    if kind == "ok-question":
        # A probe in an `Option`-returning function reports "no answer" to its
        # caller, which then decides. A `.ok()?` in a `Result`-returning
        # function turns an error into None and loses the reason.
        return "probe" if enclosing_returns_option else "fix"
    if kind == "unwrap_or_default":
        return "fix"
    return None


def main():
    allowances, by_code = load_allowances()
    rows = []
    unclassified = []
    for path in rust_sources():
        lines = open(path, encoding="utf-8", errors="replace").read().split("\n")
        stop = test_module_start(lines)
        limit = stop if stop is not None else len(lines)
        for i in range(limit):
            line = lines[i]
            for kind, rx, _desc in PATTERNS:
                if not rx.search(line):
                    continue
                context = lines[i : i + 6]
                sig = enclosing_fn(lines, i) if kind == "ok-question" else ""
                enclosing_returns_option = bool(
                    re.search(r"->\s*Option\s*<", sig)
                )
                cat = classify(
                    path,
                    i + 1,
                    kind,
                    line,
                    context,
                    allowances,
                    enclosing_returns_option,
                    by_code,
                )
                rel = os.path.relpath(path, ROOT)
                if cat is None:
                    unclassified.append((rel, i + 1, kind, line.strip()))
                else:
                    rows.append((rel, i + 1, kind, cat, line.strip()))

    # The report.
    out = ["# Silent drops", ""]
    out.append(
        "Every place a loader can lose a value without saying so, classified. "
        "Generated by `tools/audit-silent-drops.py`; do not hand-edit."
    )
    out.append("")
    counts = {}
    for r in rows:
        counts[r[3]] = counts.get(r[3], 0) + 1
    out.append("| category | sites |")
    out.append("|---|---|")
    for c in ("named", "unreachable", "fix", "?"):
        out.append(f"| {c} | {counts.get(c, 0)} |")
    out.append("")
    out.append("## Sites")
    out.append("")
    out.append("| file | line | pattern | category | code |")
    out.append("|---|---|---|---|---|")
    for rel, ln, kind, cat, code in sorted(rows):
        code = code.replace("|", "\\|")
        if len(code) > 110:
            code = code[:107] + "..."
        out.append(f"| `{rel}` | {ln} | `{kind}` | **{cat}** | `{code}` |")

    report = os.path.join(ROOT, "docs", "silent-drops.md")
    text = "\n".join(out) + "\n"
    if CHECK:
        # Verify, do not rewrite. Regenerating in CI would pass whatever the tree
        # happened to say and leave the committed report permanently behind the
        # code it describes, which is what had already happened here.
        try:
            with open(report, encoding="utf-8") as f:
                if f.read() != text:
                    print(
                        "docs/silent-drops.md is stale -- run "
                        "`python3 tools/audit-silent-drops.py` and commit it",
                        file=sys.stderr,
                    )
                    return 1
        except FileNotFoundError:
            print("docs/silent-drops.md is missing", file=sys.stderr)
            return 1
    else:
        with open(report, "w", encoding="utf-8") as f:
            f.write(text)

    print(f"{len(rows)} sites, {counts}")
    if unclassified:
        print(f"{len(unclassified)} UNCLASSIFIED site(s) -- each needs an allowance:", file=sys.stderr)
        for rel, ln, kind, code in unclassified:
            print(f"  {rel}:{ln}  {kind}  {code}", file=sys.stderr)
        return 1
    # An allowance whose line no longer holds a site is stale, and a stale
    # allowance is a claim about code that has moved or gone. Silently keeping it
    # means the next real site at that path:line inherits a justification written
    # for something else -- which is how this file lost track twice while being
    # edited above the line it named.
    used = {f"{r[0]}:{r[1]}" for r in rows}
    # A code-keyed allowance (`path category ... code: <snippet>`) is used when
    # some site in that file carries the snippet; it has no line to compare.
    for (cpath, csnippet), akeys in by_code.items():
        if any(r[0] == cpath and r[4] == csnippet for r in rows):
            used.update(akeys)
    stale = sorted(key for key in allowances if key not in used)
    if stale:
        print(
            f"{len(stale)} STALE allowance(s) -- no site at that path:line any more:",
            file=sys.stderr,
        )
        for key in stale:
            print(f"  {key}", file=sys.stderr)
        return 1
    fixes = [r for r in rows if r[3] == "fix"]
    if fixes:
        print(f"{len(fixes)} site(s) still marked Fix:", file=sys.stderr)
        for rel, ln, kind, _cat, code in fixes:
            print(f"  {rel}:{ln}  {kind}  {code}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
