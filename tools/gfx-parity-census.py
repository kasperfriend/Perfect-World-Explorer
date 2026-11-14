"""Coverage census for the effect player: what the retail `gfx.pck` actually uses.

Counts, over every `.gfx` entry of an archive:

  * element ids and the roots that carry them,
  * which roots the player renders completely,
  * how often each *parsed but unread* key is authored.

The renderer/player class sets below mirror `src/gfx_player.rs`: `RENDERED` are
the ids that reach a runtime (`emit_clock` or a dedicated one), `PARSED_ONLY`
are decoded but drawn by nothing. Unknown ids are neither.

Usage: python3 tools/gfx-parity-census.py google_drive/gfx.pck [--json out.json]
"""
import json
import re
import sys
from collections import Counter, defaultdict

sys.path.insert(0, __file__.rsplit('/', 1)[0])
import pck_lib

RENDERED = {
    100, 101, 102, 103, 104,  # sprites
    110,                       # ribbon
    120, 121, 122, 123, 124, 125,  # particles
    130,                       # light
    140,                       # cylinder
    150,                       # fractal
    151,                       # lightning
    152,                       # trail
    160,                       # model
    170,                       # sound
    180,                       # wave
    190,                       # curve
    200, 201, 202, 203, 204,   # child
    # Grids are a class of their own: `emit_clock` routes `Spec::Grid` to
    # `emit_grid`, which builds one quad per lattice cell. Listing 210/211 as
    # parsed-only under-counted coverage in this census for as long as it was
    # there; the player has drawn them since the waterfall fix.
    210, 211,                  # grid decal
}
PARSED_ONLY = set()

# Keys the player never reads, with the literal that means "not authored".
# `None` counts any value other than the default literal;
# `True` counts only a set flag ("1").
UNREAD = {
    # `NoChangeDir`, `FadeByDist` and `ShakeCam` are read by the player
    # (`advance_with_terrain`, `alpha_fade`, `root_shake`), so they are not here.
    'root': [('2DRender', True), ('2DBackLayer', True), ('PhysExist', True)],
    'any': [
        ('NoDownSample', True), ('DummyEle', None),
    ],
    'sprite': [('MaxExtent', None)],
    'ribbon': [('EnableMat', True), ('EnableOrgPos1', True), ('EnableOrgPos2', True)],
    'particle': [
        ('AlphaSeg', None), ('BetaSeg', None), ('IsAvgGen', None),
        ('AvgGenSeg', None), ('IsAvgGenSeq', None), ('ScaleNoOff', True),
    ],
    'light': [
        ('LightType', None), ('FallOff', None), ('Attenuation0', None),
        ('Attenuation1', None), ('Attenuation2', None), ('Phi', None),
        ('Theta', None),
    ],
    'fractal': [('Pos1Enable', True), ('Pos2Enable', True)],
    'lightning': [('StrokeAmp', None)],
    'trail': [('RenderSide', None)],
    'model': [('AlphaCmp', True), ('Use3DCam', True)],
    'sound': [('Force2D', True), ('IsLoop', True), ('MinDist', None), ('MaxDist', None)],
    'wave': [('EnableMat', True), ('PosEnabled', None)],
}
# Group names are the viewer's, per the class table in
# `Findings/World-effects-2026-11-09.md`: 140 is the ring that the player calls a
# cylinder, 150 the client's `A3DGFXLightning` that it calls a fractal, 180 the
# client's `A3DLtnTrail` that it calls a wave, and 152 the client's
# `A3DGFXLightningEx` that it calls a trail.
CLASS_OF_ID = {100: 'sprite', 101: 'sprite', 102: 'sprite', 103: 'sprite', 104: 'sprite',
               110: 'ribbon', 120: 'particle', 121: 'particle', 122: 'particle',
               123: 'particle', 124: 'particle', 125: 'particle', 130: 'light',
               140: 'ring', 150: 'fractal', 151: 'lightning', 152: 'trail',
               160: 'model', 170: 'sound', 180: 'wave', 190: 'curve',
               200: 'child', 201: 'child', 202: 'child', 203: 'child', 204: 'child'}

DEFAULTS = {
    # `AlphaSeg`/`BetaSeg` default to the client's 10, not to zero: 6,604 of the
    # 6,641 records that write the key write 10, and only 37 (all id 123, the
    # `IsAvgGen` population) write anything else.
    'DummyEle': '', 'MaxExtent': '0.000000', 'AlphaSeg': '10', 'BetaSeg': '10',
    'IsAvgGen': '0', 'AvgGenSeg': '0', 'IsAvgGenSeq': '0', 'LightType': '1',
    'FallOff': '0.000000', 'Attenuation0': '0.000000', 'Attenuation1': '0.000000',
    'Attenuation2': '0.000000', 'Phi': '0.000000', 'Theta': '0.000000',
    'StrokeAmp': '0.000000', 'RenderSide': '0', 'MinDist': '1.000000',
    'MaxDist': '1000.000000', 'PosEnabled': '0',
}

# Keys that cannot change the picture, each with the reason it cannot:
#   MinDist/MaxDist/IsLoop/Force2D  sound rolloff and looping, no quad
#   MaxExtent                       cull extent only (the loader derives it)
#   NoDownSample                    mip policy
#   AlphaSeg/BetaSeg                read only by `A3DEllipsoidEmitter::GenAverage`,
#                                   which runs under `IsAvgGen`, and 6,604 of the
#                                   6,641 records that write the key write the
#                                   client's default 10 (`IsAvgGen` itself stays
#                                   in the blocking set)
#   Attenuation0/FallOff/Phi/Theta  the light consumer forces
#                                   `stored_attenuation = (0, 0.4, 0)` for a
#                                   dynamic light (`scene_lights.rs:319-322`,
#                                   client `0x4250c0`) and the corpus authors
#                                   `LightType: 1` with the rest at zero
NOT_VISUAL = {'MinDist', 'MaxDist', 'MaxExtent', 'NoDownSample', 'IsLoop', 'Force2D',
              'AlphaSeg', 'BetaSeg',
              'Attenuation0', 'Attenuation1', 'Attenuation2', 'FallOff', 'Phi', 'Theta'}


SPLIT = re.compile(r'GFXELEMENTID:\s*(\d+)')


def fields(text):
    out = defaultdict(list)
    for line in text.splitlines():
        key, sep, value = line.partition(':')
        if sep:
            out[key.strip()].append(value.strip())
    return out


def authored(fields_map, key, flag_only):
    values = fields_map.get(key)
    if not values:
        return False, 0
    hits = 0
    for value in values:
        if flag_only:
            ok = value == '1'
        else:
            ok = value != DEFAULTS.get(key, '0.000000')
        hits += ok
    return hits > 0, hits


def recovery(sets, base_roots, all_roots):
    """Greedy: which single key recovers the most roots, and then the next."""
    remaining = list(sets)
    left = set().union(*sets) if sets else set()
    out = []
    covered = base_roots
    while remaining and left:
        best = max(left, key=lambda k: sum(1 for s in remaining if k in s))
        newly = [s for s in remaining if best in s]
        remaining = [s for s in remaining if best not in s]
        left = set().union(*remaining) if remaining else set()
        covered += len(newly)
        out.append({'key': best, 'new_roots': len(newly), 'cumulative': covered,
                    'percent': round(100. * covered / all_roots, 1)})
    return out


def main():
    path = sys.argv[1]
    version, entries = pck_lib.directory(path)
    gfx = [e for e in entries if e[0].lower().endswith('.gfx')]
    ids = Counter()          # element id -> elements
    id_roots = Counter()     # element id -> roots carrying it
    roots = 0
    elements = 0
    coverage = Counter()     # "all rendered" / "needs parsed-only" / "unknown id"
    coverage_elems = Counter()
    unread = Counter()       # key -> elements authoring it (non-default)
    unread_u = Counter()     # key -> elements carrying the key at all
    unread_roots = Counter()  # root flag -> roots authoring it
    unread_roots_u = Counter()
    unread_key_roots = defaultdict(set)   # key -> roots with a hit
    root_flags = Counter()   # root -> authored unread flags
    root_elem_keys = Counter()  # root -> elements with an authored unread key
    faithful_roots = 0
    faithful_elements = 0
    visual_roots = 0
    alone = Counter()
    union = Counter()
    blocked_sets = []
    visual_elements = 0

    key_sets = Counter()
    mixed = Counter()        # how many distinct class groups per root
    fail = 0

    for name, off, usize, clen in gfx:
        try:
            text = pck_lib.read_entry(path, (name, off, usize, clen)).decode('gbk', 'replace')
        except Exception:
            fail += 1
            continue
        parts = SPLIT.split(text)
        root_text = parts[0]
        pairs = list(zip(parts[1::2], parts[2::2]))
        if not pairs:
            continue
        roots += 1
        root_fields = fields(root_text)
        for entry_key, flag_only in UNREAD['root']:
            hit = authored(root_fields, entry_key, flag_only)
            unread_roots[entry_key] += hit[0]
            unread_roots_u[entry_key] += bool(root_fields.get(entry_key))
        flagged = sum(1 for entry_key, flag_only in UNREAD['root']
                      if authored(root_fields, entry_key, flag_only)[0])
        root_flags[flagged] += 1
        touched = 0
        visual_touched = set()
        seen = set()
        rendered = True
        parsed = False
        unknown = False
        for id_text, body in pairs:
            element_id = int(id_text)
            elements += 1
            ids[element_id] += 1
            seen.add(element_id)
            if element_id in PARSED_ONLY:
                parsed = True
                rendered = False
            elif element_id not in RENDERED:
                unknown = True
                rendered = False
            element_fields = fields(body)
            groups = ['any']
            if CLASS_OF_ID.get(element_id):
                groups.append(CLASS_OF_ID[element_id])
            hit_here = False
            for group in groups:
                for entry_key, flag_only in UNREAD.get(group, []):
                    hit, occurrences = authored(element_fields, entry_key, flag_only)
                    unread_u[entry_key] += bool(element_fields.get(entry_key))
                    if hit:
                        hit_here = True
                        unread[entry_key] += 1
                        unread_key_roots[entry_key].add(name)
                        if entry_key not in NOT_VISUAL:
                            visual_touched.add(entry_key)
            if hit_here:
                touched += 1
        if touched:
            root_elem_keys[touched] += 1
        for element_id in seen:
            id_roots[element_id] += 1
        groups = {CLASS_OF_ID.get(i, 'unknown') for i in seen}
        mixed[len(groups)] += 1
        if unknown:
            coverage['unknown id present'] += 1
            coverage_elems['unknown id present'] += len(pairs)
        elif parsed:
            coverage['parsed-only present'] += 1
            coverage_elems['parsed-only present'] += len(pairs)
        else:
            coverage['fully rendered'] += 1
            coverage_elems['fully rendered'] += len(pairs)
            if not flagged and not touched:
                faithful_roots += 1
                faithful_elements += len(pairs)
            visual_flag = sum(1 for entry_key, flag_only in UNREAD['root']
                              if entry_key != 'NoChangeDir'
                              and authored(root_fields, entry_key, flag_only)[0])
            if not visual_flag and not visual_touched:
                visual_roots += 1
                visual_elements += len(pairs)
            blocked = frozenset(visual_touched - NOT_VISUAL)
            if blocked:
                blocked_sets.append(blocked)
                for one in blocked:
                    union[one] += 1
                if len(blocked) == 1:
                    alone[next(iter(blocked))] += 1
            if visual_flag or visual_touched:
                flags = tuple(sorted(f'root:{k}' for k, f in UNREAD['root']
                                     if f and authored(root_fields, k, f)[0]))
                key_sets[(flags, tuple(sorted(visual_touched)))] += 1

    summary = {
        'archive': path.split('/')[-1],
        'pack_version': hex(version),
        'roots': roots,
        'elements': elements,
        'unreadable_entries': fail,
        'coverage_roots': dict(coverage),
        'coverage_elements': dict(coverage_elems),
        'faithful_roots': faithful_roots,
        'faithful_elements': faithful_elements,
        'visual_roots': visual_roots,
        'visual_elements': visual_elements,

        'recovery_greedy': recovery(blocked_sets, visual_roots, roots),
        'unresolved_alone': {k: alone[k] for k in sorted(union, key=lambda k: -union[k])},
        'unresolved_union': {k: union[k] for k in sorted(union, key=lambda k: -union[k])},
        'unresolved_key_sets': [
            {'root_flags': list(flags), 'element_keys': list(keys), 'roots': count}
            for (flags, keys), count in key_sets.most_common(40)],
        'roots_by_unread_flag_count': dict(sorted(root_flags.items())),
        'roots_by_unread_key_element_count': dict(sorted(root_elem_keys.items())),
        'classes_per_root': dict(sorted(mixed.items())),
        'element_ids': {str(k): {'elements': v, 'roots': id_roots[k]}
                        for k, v in ids.most_common()},
        'unread_keys': {k: {'elements_authored': unread[k], 'elements_present': unread_u[k],
                            'roots_affected': len(unread_key_roots[k])}
                        for k in sorted(unread)},
        'unread_root_flags': {k: {'authored': unread_roots[k], 'present': unread_roots_u[k]}
                              for k in sorted(unread_roots)},
    }
    print(json.dumps(summary, ensure_ascii=False, indent=1))
    if '--json' in sys.argv:
        out = sys.argv[sys.argv.index('--json') + 1]
        with open(out, 'w', encoding='utf-8') as handle:
            json.dump(summary, handle, ensure_ascii=False, indent=1)
        print('wrote', out, file=sys.stderr)


if __name__ == '__main__':
    main()
