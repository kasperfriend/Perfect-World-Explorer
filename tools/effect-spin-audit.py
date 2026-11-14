"""Sweep a published effect run for keypoint 2D spins, class by class.

The client folds a keypoint's `Rad_2D` into the element matrix itself
(`GetTranMatrix`, A3DGFXKeyPoint.h:286-293):

    if (kp.m_fRad2D) (kp.m_vDir * A3DQUATERNION(vAxis, kp.m_fRad2D)).ConvertToMatrix(matTran);
    else kp.m_vDir.ConvertToMatrix(matTran);
    matTran.SetRow(3, kp.m_vPos);

so *every* drawing class inherits it, about `vAxis`: `_unit_z` by default and
`_unit_y` for the ring (`A3DGFXRing.cpp:265`/`:315`). The viewer had it only in
the card arms (`gfx_player::quad_corners`), which is why a spinning model stood
still (§57.19). This tool reads the frame channel a preview run publishes and
prints, per root and per element id, how many sampled frames carry a spin and
how large it gets, next to the axis the client would turn — so the next run says
in one line which classes are affected and whether the class is in the table.

    python3 tools/effect-spin-audit.py [dir]      # default Findings/effect-preview

A class that authors a spin but is missing from `CLASSES` is the one thing that
must fail: it means a draw path nobody has checked against `GetTranMatrix`.
"""

import os
import re
import sys
from collections import defaultdict

# id -> (class, spin axis, where the client builds the element matrix)
CLASSES = {
    100: ("DECAL_3D", "card", "A3DDecalEx.cpp:649/973 — card arms spin in their own plane"),
    101: ("DECAL_2D", "card", "A3DDecalEx.cpp:1208 — screen billboard, view forward"),
    102: ("DECAL_BILLBOARD", "card", "A3DDecalEx.cpp:1089 — card arms"),
    110: ("TRAIL", "Z", "A3DTrail.cpp:560"),
    120: ("PARTICLE_POINT", "Z", "A3DParticleSystemEx.cpp:996 — the particle's own quaternion"),
    121: ("PARTICLE_BOX", "Z", "A3DParticleSystemEx.cpp:996"),
    122: ("PARTICLE_MULTIPLANE", "Z", "A3DParticleSystemEx.cpp:996"),
    123: ("PARTICLE_CYLINDER", "Z", "A3DParticleSystemEx.cpp:996"),
    124: ("PARTICLE_ELLIPSOID", "Z", "A3DParticleSystemEx.cpp:996"),
    125: ("PARTICLE_CURVE", "Z", "A3DParticleSystemEx.cpp:996"),
    130: ("LIGHT", "none", "A3DGFXLight draws no geometry"),
    140: ("RING", "Y", "A3DGFXRing.cpp:265/315 — GetTranMatrix(kp, _unit_y)"),
    150: ("LIGHTNING", "Z", "A3DGFXLightning.cpp:857"),
    152: ("LIGHTNING_EX", "Z", "A3DLtnTrail.cpp:172"),
    160: ("MODEL", "Z", "A3DGFXModel.cpp:233"),
    170: ("SOUND", "none", "no geometry"),
    180: ("LTNTRAIL", "Z", "A3DLtnTrail.cpp:172 / A3DTrail.cpp:560"),
    190: ("PARABOLOID", "Z", "A3DParaboloid.cpp:201/251"),
    200: ("CONTAINER", "Z", "A3DGFXContainer.cpp:300 — a child root takes the spun parent"),
    210: ("GRID_DECAL", "Z", "A3DGridDecalEx.cpp:445/477"),
    230: ("ECMODEL", "Z", "A3DGFXECModel.cpp:352"),
}

LINE = re.compile(r"\] (\S+) id=(\d+) kp=(\d+).*? rot=(-?[\d.]+)")


def parse(directory):
    path = os.path.join(directory, "effect-frames.log.txt")
    if not os.path.exists(path):
        raise SystemExit(f"no frame channel at {path}")
    rows = []
    for line in open(path, encoding="utf-8", errors="replace"):
        match = LINE.search(line)
        if not match:
            continue
        root, element, key, rot = match.groups()
        rows.append((root, int(element), int(key), float(rot)))
    if not rows:
        # The channel writes `rot=` only past its gate; a run with the channel
        # off is a configuration mistake, not a clean sweep.
        raise SystemExit(f"{path} carries no `rot=` field")
    return rows


def main(argv):
    directory = argv[1] if len(argv) > 1 else os.path.join("Findings", "effect-preview")
    rows = parse(directory)

    per_class = defaultdict(lambda: [0, 0, 0.0])
    per_root = defaultdict(lambda: [0, 0, 0.0])
    for root, element, _key, rot in rows:
        under = 1 if abs(rot) > 1e-6 else 0
        for bucket in (per_class[element], per_root[root]):
            bucket[0] += 1
            bucket[1] += under
            bucket[2] = max(bucket[2], abs(rot))

    print(f"{len(rows)} sampled element frames, {len(per_class)} ids, {len(per_root)} roots\n")
    print(f"{'id':>4}  {'class':<18} {'axis':<5} {'frames':>6} {'spun':>5} {'max |rot|':>9}  matrix")
    unknown = []
    for element in sorted(per_class):
        frames, spun, peak = per_class[element]
        entry = CLASSES.get(element)
        if entry is None:
            unknown.append(element)
            name, axis, where = "??", "??", "not in the audit's table"
        else:
            name, axis, where = entry
        print(f"{element:>4}  {name:<18} {axis:<5} {frames:>6} {spun:>5} {peak:>9.3f}  {where}")

    print("\nper root")
    for root in sorted(per_root):
        frames, spun, peak = per_root[root]
        print(f"  {root:<48} frames={frames:<4} spun={spun:<4} max={peak:.3f}")

    spun_ids = sorted(element for element, (_f, spun, _p) in per_class.items() if spun)
    print(f"\nids that authored a spin in this run: {spun_ids or 'none'}")
    if unknown:
        print(f"\nFAIL: ids outside the class table: {sorted(unknown)}")
        return 1
    print("every id in the sweep has a client axis on record")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
