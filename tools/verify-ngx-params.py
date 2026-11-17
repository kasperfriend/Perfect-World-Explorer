#!/usr/bin/env python3
"""Check the viewer's NGX parameter writes against NVIDIA's own headers.

The NGX headers in `NVIDIA/DLSS` (`include/nvsdk_ngx_defs*.h` and
`include/nvsdk_ngx_helpers*_d3d.h`, tag `v310.9.16`) spell every parameter name
and, in the inline `NGX_D3D12_CREATE_DLSS_EXT` / `NGX_D3D12_EVALUATE_DLSS_EXT`
helpers, the setter each name is written with. The viewer builds its parameter
map by hand (the SDK DLLs export the feature entry points but no parameter
allocator), so a mistyped name or a `SetUI` where NVIDIA writes `SetI` is a
silent refusal, not a compile error. This script compares the two directly.

Usage:
  python3 tools/verify-ngx-params.py                     # offline, vendored table
  python3 tools/verify-ngx-params.py --headers DIR       # re-derive from the headers
  python3 tools/verify-ngx-params.py --headers DIR --dump-fixture F

The offline table is `tools/fixtures/ngx-v310.9.16-params.json`, generated from
the `v310.9.16` headers with `--dump-fixture`. CI runs the offline form, so a
mistyped name or a wrong setter fails the build without a network fetch.

`DIR` must contain the headers from the `v310.9.16` tag, e.g.

  for f in nvsdk_ngx_defs.h nvsdk_ngx_helpers_d3d.h \\
           nvsdk_ngx_defs_dlssg.h nvsdk_ngx_helpers_dlssg_d3d.h \\
           nvsdk_ngx.h nvsdk_ngx_params.h nvsdk_ngx_helpers.h; do
    gh api "repos/NVIDIA/DLSS/contents/include/$f?ref=v310.9.16" \\
      -H "Accept: application/vnd.github.raw" > "DIR/$f"
  done

Exit status: 0 when every name exists and every setter type conforms, 1
otherwise. Names the helpers never write (queries, capability keys) are
reported as informational, not as failures.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

SETTER_TO_RUST = {
    "SetUI": "u32",
    "SetI": "i32",
    "SetF": "f32",
    "SetD3d12Resource": "d3d12_resource",
    "SetVoidPointer": "void_pointer",
}
RUST_TO_SETTER = {kind: name for name, kind in SETTER_TO_RUST.items()}

# The DLSS-G keys are `NVSDK_NGX_DLSSG_Parameter_*` in `nvsdk_ngx_defs_dlssg.h`;
# everything else is `NVSDK_NGX_Parameter_*`.
SYMBOL = r"NVSDK_NGX_(?:DLSSG_)?Parameter_[A-Za-z0-9_]+"
DEFINE = re.compile(r'#define\s+(' + SYMBOL + r')\s+"([^"]*)"')
ALIAS = re.compile(
    r"#define\s+(" + SYMBOL + r")\s+(" + SYMBOL + r")\s*$"
)
CALL = re.compile(
    r"NVSDK_NGX_Parameter_(Set[A-Za-z0-9]+)\s*\(\s*[A-Za-z0-9_]+\s*,\s*(" + SYMBOL + r")",
    re.S,
)
VIRTUAL = re.compile(
    r"virtual\s+[A-Za-z0-9_\s]+?\s(Set|Get)\s*\(\s*const\s+char\s*\*\s*\w+\s*,\s*([^)]*?)\)",
)
VALUE_TYPES = [
    ("unsigned long long", "u64"),
    ("double", "f64"),
    ("float", "f32"),
    ("unsigned int", "u32"),
    ("ID3D11Resource", "d3d11"),
    ("ID3D12Resource", "d3d12"),
    ("void", "void"),
    ("int", "i32"),
]
RUST_SLOT = re.compile(r"const\s+SLOT_([A-Z0-9_]+)\s*:\s*usize\s*=\s*(\d+)\s*;")
RUST_CONST = re.compile(r'const\s+(P_[A-Z0-9_]+)\s*:\s*&str\s*=\s*"([^"]*)"')
RUST_ARRAY = re.compile(r"const\s+(P_[A-Z0-9_]+)\s*:\s*\[&str;\s*\d+\]\s*=\s*\[(.*?)\]", re.S)
RUST_ITEM = re.compile(r"P_[A-Z0-9_]+")
RUST_WRITE = re.compile(r"\.set_([a-z0-9_]+)\(\s*(P_[A-Z0-9_]+)")


# The vtable order the *driver core* implements, which is what our slots have to
# match. It is the reverse of the declaration in `nvsdk_ngx_params.h:55-73`, and
# that is not a guess: a session wrote one real `ID3D12Resource *` and one
# distinctive integer through *all eight* declared `Set` overloads and read them
# back through every reader, and all sixteen slots fit this order and none fits
# the declaration's (`docs/blockers.md` B-27). A vtable is an ABI, so addressing
# the declaration's slot 6 (`Set(ID3D12Resource *)`) addresses `Set(float)` on
# this core — which is how four evaluate resources were stored as numbers while
# NGX kept reporting `could not find Color parameter`.
MEASURED_SLOTS = {
    "SET_VOID": 0,
    "SET_D3D12": 1,
    "SET_D3D11": 2,
    "SET_I32": 3,
    "SET_U32": 4,
    "SET_F64": 5,
    "SET_F32": 6,
    "SET_U64": 7,
    "GET_VOID": 8,
    "GET_D3D12": 9,
    "GET_D3D11": 10,
    "GET_I32": 11,
    "GET_U32": 12,
    "GET_F64": 13,
    "GET_F32": 14,
    "GET_U64": 15,
}


def header_vtable(paths):
    """Slot index -> `Set`/`Get` of which value type, from nvsdk_ngx_params.h.

    `NVSDK_NGX_Parameter` is an abstract C++ class, so its virtual methods are
    the ABI: the n-th declared method is vtable slot n. MSVC emits no destructor
    slot for a class that declares no destructor, which is why `Reset()` lands
    last and the setters start at 0.
    """
    for path in paths:
        text = path.read_text(errors="replace")
        start = text.find("typedef struct NVSDK_NGX_Parameter")
        if start < 0:
            continue
        block = text[start : text.find("NVSDK_NGX_Parameter;", start)]
        slots = {}
        for index, match in enumerate(VIRTUAL.finditer(block)):
            method, tail = match.group(1), match.group(2)
            kind = next((kind for token, kind in VALUE_TYPES if token in tail), "?")
            slots[f"{method.upper()}_{kind.upper()}"] = index
        if slots:
            return slots
    return {}


def header_tables(paths):
    names, setters = {}, {}
    for path in paths:
        text = path.read_text(errors="replace")
        for symbol, value in DEFINE.findall(text):
            names[symbol] = value
        for symbol, target in ALIAS.findall(text):
            if target in names:
                names[symbol] = names[target]
        for setter, symbol in CALL.findall(text):
            if setter in SETTER_TO_RUST and symbol in names:
                setters.setdefault(names[symbol], set()).add(SETTER_TO_RUST[setter])
    return names, setters


def rust_writes(paths):
    """Resolve every `set_*(P_*)` to its string, per file.

    Resolution is per file on purpose: `imp.rs` and `fg.rs` both declare a
    `P_RESET`, and one is `Reset` (an int for the SR feature) while the other is
    `DLSSG.Reset` (a uint for frame generation). A single global table would
    blame one file's write on the other file's name.
    """
    consts, arrays, resolved = {}, {}, []
    for path in paths:
        text = path.read_text()
        local_consts, local_arrays = {}, {}
        for symbol, value in RUST_CONST.findall(text):
            local_consts[symbol] = value
        for symbol, body in RUST_ARRAY.findall(text):
            local_arrays[symbol] = RUST_ITEM.findall(body)
        consts.update(local_consts)
        arrays.update(local_arrays)
        for kind, symbol in RUST_WRITE.findall(text):
            targets = local_arrays.get(symbol)
            if targets is None:
                targets = [symbol] if symbol in local_consts else []
            for target in targets:
                resolved.append((path.name, kind, local_consts.get(target, target), target))
    return consts, arrays, resolved


DEFAULT_FIXTURE = pathlib.Path("tools/fixtures/ngx-v310.9.16-params.json")
HEADER_TAG = "v310.9.16"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--headers", type=pathlib.Path)
    parser.add_argument("--fixture", type=pathlib.Path, default=DEFAULT_FIXTURE)
    parser.add_argument("--dump-fixture", type=pathlib.Path)
    parser.add_argument(
        "--source",
        type=pathlib.Path,
        nargs="+",
        default=[
            pathlib.Path("src/dlss/imp.rs"),
            pathlib.Path("src/dlss/fg.rs"),
            pathlib.Path("src/dlss/params.rs"),
        ],
    )
    args = parser.parse_args(argv)

    if args.headers is not None:
        headers = sorted(args.headers.glob("*.h"))
        if not headers:
            return f"no headers in {args.headers}", 1
        names, setters = header_tables(headers)
        vtable = header_vtable(headers)
        if args.dump_fixture is not None:
            args.dump_fixture.parent.mkdir(parents=True, exist_ok=True)
            args.dump_fixture.write_text(
                json.dumps(
                    {
                        "source": f"NVIDIA/DLSS include/, tag {HEADER_TAG}",
                        "names": dict(sorted(names.items())),
                        "setters": {k: sorted(v) for k, v in sorted(setters.items())},
                        "vtable": dict(sorted(vtable.items(), key=lambda kv: kv[1])),
                    },
                    indent=1,
                )
                + "\n"
            )
            print(f"wrote {args.dump_fixture}")
    elif args.fixture.is_file():
        data = json.loads(args.fixture.read_text())
        names = data["names"]
        setters = {k: set(v) for k, v in data["setters"].items()}
        vtable = data["vtable"]
    else:
        return f"no --headers and no fixture at {args.fixture}", 1
    consts, _, writes = rust_writes(args.source)
    if not consts:
        return f"no P_* constants found in {', '.join(map(str, args.source))}", 1

    unknown, mismatched, unchecked = [], [], set()
    for origin, kind, name, symbol in writes:
        if name not in names.values():
            unknown.append(f"{origin}: {symbol} = {name!r} is not a parameter name in the headers")
            continue
        expected = setters.get(name)
        if not expected:
            unchecked.add(name)
        elif kind not in expected:
            mismatched.append(
                f"{origin}: {symbol} ({name}) written with set_{kind} "
                f"({RUST_TO_SETTER.get(kind, '?')}); the headers write it with "
                f"{', '.join(sorted(RUST_TO_SETTER[k] for k in expected))}"
            )

    slot_mismatch = []
    reversed_note = 0
    slots = {}
    for path in args.source:
        for kind, index in RUST_SLOT.findall(path.read_text()):
            slots[kind] = int(index)
    for kind, index in sorted(slots.items(), key=lambda kv: kv[1]):
        measured = MEASURED_SLOTS.get(kind)
        if measured is None:
            slot_mismatch.append(f"SLOT_{kind} = {index}: no such overload on the core")
        elif measured != index:
            slot_mismatch.append(
                f"SLOT_{kind} = {index}, but the measured core order puts {kind} "
                f"at slot {measured} (docs/blockers.md B-27)"
            )
        elif vtable.get(kind) is not None and vtable[kind] != index:
            # The header prints the reverse order; that is expected, and the
            # measured one is what the code must use.
            reversed_note += 1
    if vtable:
        missing = sorted(set(vtable) - set(MEASURED_SLOTS))
        extra = sorted(set(MEASURED_SLOTS) - set(vtable))
        for kind in missing:
            slot_mismatch.append(f"the core has no measured slot for {kind}, but the header declares one")
        for kind in extra:
            slot_mismatch.append(f"measured slot {kind} has no counterpart in the header")
        print(
            f"vtable: {len(vtable)} virtual methods in the header, "
            f"{len(slots)} slots used by params.rs, "
            f"{reversed_note} deliberately reversed vs the header"
        )
    for line in slot_mismatch:
        print(f"ERROR   {line}")

    used = {name for _, _, name, _ in writes}
    source = (
        f"{len(sorted(args.headers.glob('*.h')))} header files"
        if args.headers is not None
        else str(args.fixture)
    )
    print(f"table: {source}, {len(names)} parameter names")
    print(f"sources: {len(consts)} P_* string constants, {len(writes)} writes, {len(used)} distinct names")
    for line in unknown:
        print(f"ERROR   {line}")
    for line in mismatched:
        print(f"ERROR   {line}")
    for name in sorted(unchecked):
        print(f"note    {name}: the create/evaluate helpers never set it (query or capability key)")
    if unknown or mismatched or slot_mismatch:
        print(
            f"FAILED: {len(unknown)} unknown, {len(mismatched)} mistyped, "
            f"{len(slot_mismatch)} bad vtable slots"
        )
        return None, 1
    print(
        f"OK: every name exists in the headers, every setter type conforms "
        f"({len(writes)} writes)"
        + (", every vtable slot matches the measured core order" if vtable else "")
        + f" ({len(slots)} slots checked against the measured core order)"
    )
    return None, 0


if __name__ == "__main__":
    message, status = main()
    if message:
        print(message)
    sys.exit(status)
