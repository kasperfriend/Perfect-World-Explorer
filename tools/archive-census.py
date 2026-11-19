#!/usr/bin/env python3
"""Name every member of a retail pack and say whether the viewer handles it.

P3.01 asks for "an archive-wide survey that names what is still unsupported"
rather than another sample. This walks a whole pack and classifies every single
member, so "unsupported" stops being a feeling and becomes a list with names,
sizes and a reason for each entry.

Classification is by *content signature*, not by file extension alone, because
several extensions are containers that the client treats differently per member
(for example `.tga` in `shaders.pck` holds icons, not shader data).

Usage:
    archive-census.py PACK [--json OUT] [--list-unsupported]

Exit status is 0 even when unsupported members are found; this is a census, not
a gate.
"""
import argparse
import collections
import json
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pck_lib as pck  # noqa: E402

# Extensions whose contents `src/` demonstrably parses, with the module that
# does it. Anything absent from this table is unsupported by definition, and the
# census says so by name instead of dropping it.
HANDLED = {
    "dds": ("DDS", "src/dxt.rs, src/texture_resolution.rs"),
    "mox": ("MOXB", "src/grass.rs"),
    "mod": ("MOXTMODEL", "src/grass.rs (mox_wrapper_child)"),
    "bmd": (None, "src/bmd.rs, src/objects.rs"),
    "ecm": ("MOXTVERSION: ", "src/ecmodel.rs, src/world_models.rs"),
    "ski": ("MOXBIKSA", "src/skin.rs"),
    "bon": ("MOXBEKSA", "src/ecmodel.rs"),
    "smd": ("MOXBDMSA", "src/ecmodel.rs"),
    "stck": ("MOXBKCTS", "src/animation.rs"),
    "spt": ("SPT", "src/tree_source.rs"),
    "tga": ("TGA", "src/objects.rs, src/light_consumers.rs"),
    "bmp": ("BMP", "src/sky_gpu.rs, src/texture_resolution.rs"),
    "txt": (None, "src/post_gpu.rs (HLSL -> WGSL), src/post_plan.rs"),
    "sdr": (None, "src/post_gpu.rs"),
    "action": (None, "src/ecmodel.rs, src/model_playback.rs"),
    "ogg": ("OggS", "src/music.rs"),
    # Both skills and effect graphs are the same container: they start
    # `MOXTVersion:` and `src/gfx_file.rs` parses them, `.sgc` as a version-4
    # record with `Path:`/`ActionCount` keys and `.gfx` with the full element
    # grammar. Leaving them out of this table reported 2,439 members of the
    # staged `gfx.pck` as "no parser in src/" when they are the one format that
    # pack exists for -- the census was measuring its own table, not the code.
    "gfx": ("MOXTVERSION: ", "src/gfx_file.rs, src/gfx_owner.rs"),
    "sgc": ("MOXTVERSION: ", "src/gfx_file.rs"),
}
# Extensions whose *content* is a format `src/` reads but whose name is not the
# extension that format ships under. `gfx/怪物/通用/地狱狮王效果` has no extension
# and holds a `MOXTVersion: 41` effect graph, so classifying by name alone
# reported a parsed file as unsupported. A member that sniffs as a handled
# signature is classified by the signature and says so.
SNIFF_ONLY = {
    "MOXTVERSION: ": ("gfx", "src/gfx_file.rs (extensionless effect graph)"),
    "MOXB": ("fw", "no src/ reader (fireworks only); children are FW* records"),
}
# Formats that belong to the client but not to the renderer, with the reason
# they are out of scope. These are reported separately from real gaps.
OUT_OF_SCOPE = {
    "ini": "configuration; parsed by the launcher/settings layer, not by src/",
    "cfg": "configuration/table data, consumed ad hoc",
    "lst": "list data, consumed ad hoc",
    "sw": "client settings blob",
    "border": "UI border description",
    "data": "opaque data blob",
    "scc": "source control metadata shipped by mistake in the retail pack",
    "exe": "prebuilt binaries shipped inside the shader pack",
    "vso": "precompiled vertex shader object; superseded by the HLSL sources",
    "grs": "test-only grass sample; absent from elementclient.exe",
    "tbh": "tree billboard helper, consumed with the .spt tree source",
    "mphy": "per-model physics/ragdoll blob; no renderer consumer",
    "his": "build history metadata (HIS) shipped inside the pack",
    "log": "authoring log files shipped inside the pack",
    "tmp": "authoring leftover, not a runtime asset",
    "rar": "archive shipped inside the pack by mistake",
    "bip": "3ds Max Character Studio Biped clip, authoring source",
    "dll": "prebuilt binary shipped inside the pack",
    "spt": "SpeedTree source; converted by tools/speedtree/bake.cpp against the retail "
           "SpeedTreeRT.dll (tools/speedtree/speedtree-bake.exe), not parsed by src/",
    "ecp": "ECModel convex collision hull (`MOXBpbce` + a path back to its .ecm); "
           "collision data, not rendered geometry",
}
SUPPORTED = {"dds", "mox", "mod", "bmd", "ecm", "ski", "bon", "smd", "stck",
             "tga", "bmp", "txt", "sdr", "action", "ogg", "gfx", "sgc"}


def sniff(b):
    if len(b) >= 4 and b[:4] == b"DDS ":
        return "DDS"
    # Longest match first: every Angelica binary container starts "MOXB", so a
    # bare 4-byte MOXB test placed above these would swallow `ski`, `bon`, `smd`
    # and `stck` as generic models. That is exactly the shape of the bug this
    # ordering fixes.
    for tag in (b"MOXBIKSA", b"MOXBEKSA", b"MOXBDMSA", b"MOXBKCTS"):
        if b[:8] == tag:
            return tag.decode()
    if len(b) >= 9 and b[:9] == b"MOXTMODEL":
        return "MOXTMODEL"
    if len(b) >= 12 and b[:12].upper() == b"MOXTVERSION:":
        return "MOXTVERSION: "
    if len(b) >= 4 and b[:4] == b"MOXB":
        return "MOXB"
    if len(b) >= 4 and b[:4] == b"OggS":
        return "OGG"
    if len(b) >= 2 and b[:2] == b"BM":
        return "BMP"
    if len(b) >= 3 and b[:3] == b"SPT":
        return "SPT"
    if len(b) >= 3 and b[:3] in (b"\0\0\x02",):
        return "TGA"
    return None


def ext_of(name):
    return name.rsplit(".", 1)[-1].lower() if "." in name else ""


def census(path):
    try:
        version, entries = pck.directory(path)
    except pck.EncryptedPackage as e:
        # Not a parse failure and not a gap in the parser table: this archive
        # uses a feature the reader lacks. Saying so by name keeps the census
        # from reporting a shipped pack as damaged, and keeps it out of the
        # "unsupported members" list, which is about formats, not containers.
        print(f"SKIPPED: {path}: {e}")
        return None, []
    rows = []
    for name, offset, size, flag in entries:
        ext = ext_of(name)
        row = {"name": name, "ext": ext, "size": size, "flag": flag,
               "pack_version": version}
        if size <= 0:
            row.update(status="unsupported", reason="empty member")
            rows.append(row)
            continue
        sig = None
        # Sniff everything: an extensionless member can still be a format we
        # read, and a wrong extension is exactly what this census is for.
        if True:
            sig = sniff(pck.read_entry(path, (name, offset, size, flag))[:16])
        row["signature"] = sig
        if ext in SUPPORTED:
            want = HANDLED.get(ext, (None, ""))[0]
            if want is not None and sig != want:
                row.update(status="unsupported",
                           reason=f"expected {want} content, found {sig or 'unknown'}")
            else:
                row.update(status="supported", where=HANDLED.get(ext, (None, "?"))[1])
        elif ext in OUT_OF_SCOPE:
            row.update(status="out-of-scope", reason=OUT_OF_SCOPE[ext])
        elif sig in SNIFF_ONLY:
            ext_by_content, where = SNIFF_ONLY[sig]
            row.update(status="supported" if where.startswith("src/") else "unsupported",
                       where=where,
                       reason=f"content is {sig!r} (the {ext_by_content} format), "
                              f"not the extension's own format")
        else:
            row.update(status="unsupported",
                       reason="no parser in src/ for this extension")
        rows.append(row)
    return version, rows


def summarize(rows):
    counts = collections.Counter(r["status"] for r in rows)
    by_ext = collections.defaultdict(collections.Counter)
    for r in rows:
        by_ext[r["ext"] or "(none)"][r["status"]] += 1
    return counts, by_ext


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("pack")
    ap.add_argument("--json")
    ap.add_argument("--list-unsupported", action="store_true")
    args = ap.parse_args(argv)

    version, rows = census(args.pack)
    if version is None:
        # `census` already said why. Exit 0: the pack is out of this reader's
        # reach, which is not a failed census.
        return 0
    counts, by_ext = summarize(rows)
    print(f"{args.pack}  version {version:#x}  members {len(rows)}")
    for status in ("supported", "out-of-scope", "unsupported"):
        print(f"  {status:12s} {counts[status]:6d}")
    print()
    print(f"  {'ext':8s} {'supported':>10s} {'out-of-scope':>13s} {'unsupported':>12s}")
    for ext in sorted(by_ext, key=lambda e: (-sum(by_ext[e].values()), e)):
        c = by_ext[ext]
        print(f"  {ext:8s} {c['supported']:10d} {c['out-of-scope']:13d} {c['unsupported']:12d}")
    bad = [r for r in rows if r["status"] == "unsupported"]
    if bad:
        print()
        print(f"  unsupported members ({len(bad)}):")
        shown = bad if args.list_unsupported else bad[:20]
        for r in shown:
            print(f"    {r['size']:9d}  {r['name']}  ({r['reason']})")
        if len(shown) < len(bad):
            print(f"    ... and {len(bad) - len(shown)} more; use --list-unsupported")
    if args.json:
        with open(args.json, "w") as fh:
            json.dump({"pack": args.pack, "version": version,
                       "counts": dict(counts), "members": rows}, fh, indent=1)
        print(f"\n  json -> {args.json}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
