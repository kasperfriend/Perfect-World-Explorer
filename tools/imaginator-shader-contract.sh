#!/usr/bin/env bash
# The DXIL trace shader's output channels must match the CPU tracer's `pack`.
#
# The composite reads rgba32float as: rgb = colour radiance, a = sun visibility,
# negative when the primary ray escaped. The shader once wrote
# (reflection, indirect, visibility, distance) instead, so the composite read a
# hit distance as sun visibility and every traced surface stayed fully lit. That
# is invisible in a status line and was only found by reading the shader against
# `Tracer::pack`, so it is pinned here.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SHADER="$REPO/tools/imaginator-shaders/dxr_trace.hlsl"
PACKER="$REPO/src/imaginator/tracer.rs"
status=0

fail() { echo "imaginator-shader-contract: $*" >&2; status=1; }

[ -f "$SHADER" ] || { echo "no shader at $SHADER" >&2; exit 1; }

# 1. The CPU contract this mirrors.
grep -q 'let radiance = s.reflection + s.indirect \* s.occlusion;' "$PACKER" \
  || fail "Tracer::pack no longer publishes colour+visibility; update this check and the shader"

# 2. The shader writes colour into rgb and visibility into a, with the same
#    negative-for-sky signal.
grep -q 'float3 radiance = (reflection + indirect \* mean_visibility + lamps) \* inv;' "$SHADER" \
  || fail "the shader must include reflection, indirect*visibility and direct lamps"
grep -q 'hits > 0.0 ? mean_visibility : -1.0' "$SHADER" \
  || fail "the shader's alpha must be sun visibility, and -1 for a ray that escaped"

# 3. The two radiance accumulators must be colour, not luminance: a scalar here
#    flattens every traced surface to one tint.
grep -q 'float3 reflection = float3(0.0, 0.0, 0.0);' "$SHADER" \
  || fail "reflection must accumulate as float3"
grep -q 'float3 indirect = float3(0.0, 0.0, 0.0);' "$SHADER" \
  || fail "indirect must accumulate as float3"

# 4. Both tracers must weight the mirror term by the surface's own reflectance.
#    Neither did: flat terrain (`specular` 0.03) added a full mirror of the sky,
#    and the composite added that on top of a frame that already carried the
#    sky, so the ground rendered white. A sky reflection is now multiplied by
#    how reflective the surface actually is, in both tracers.
grep -q 'reflection = reflection \* reflectivity;' "$PACKER" \
  || fail "the CPU tracer must weight its reflection by the surface's specular"
# Both tracers must weight a bounce by the albedo of the surface it lands on.
# Omitting it treats every bounce surface as a perfect white reflector and
# over-brightens by 1/albedo - terrain at 0.22 came out 4.5x too bright, which
# is the white ground.
grep -q 'indirect += weight \* a \* incoming;' "$PACKER" \
  || fail "the CPU tracer must weight a bounce by the hit surface's albedo"
grep -q 'indirect += weight \* material_of(h.primitive).rgb \* incoming;' "$SHADER" \
  || fail "the DXIL shader must weight a bounce by the hit surface's albedo"

# 5. The water switch has to reach every tracer. It was a checkbox that was
#    serialised, drawn, and read by nothing - toggling it changed no pixel.
grep -q 'TERM_WATER_FILL' "$SHADER" \
  || fail "the DXIL shader must honour the water reflection switch"
grep -q 'water_reflection_fill = options.water_reflection_fill;' "$PACKER" \
  || fail "the CPU tracer must honour the water reflection switch"
grep -q 'water_reflection_fill: settings.water_reflection_fill' "$PACKER" \
  || fail "TraceOptions must carry the water switch from the settings"

if ! grep -q 'float3 through = float3(mat.a, mat.a, mat.a);' "$SHADER"; then
  fail "the DXIL shader must start its mirror chain at the surface's own specular, not 1.0"
fi

# 5. Shapes DXC refuses outright, which cost several CI rounds to discover.
if python3 - "$SHADER" <<'PY'
import re, sys
s = open(sys.argv[1]).read()
bad = []
for m in re.finditer(r'\n[\w\s\*]*?\b(\w+)\s*\([^;{]*\)\s*\{', s):
    name = m.group(1)
    if name in ('if', 'for', 'while', 'switch', 'return'):
        continue
    i, depth, j = m.end(), 1, m.end()
    while j < len(s) and depth > 0:
        depth += 1 if s[j] == '{' else (-1 if s[j] == '}' else 0)
        j += 1
    if re.search(r'\b' + re.escape(name) + r'\s*\(', s[i:j]):
        bad.append(name)
if bad:
    print("recursive functions: " + ", ".join(bad))
    sys.exit(1)
sys.exit(0)
PY
then :; else fail "DXC rejects recursion; walk the chain with a loop"; fi

RESERVED='point|triangle|line|sample|matrix|vector|texture|compile|pass'
if grep -nqE "\b(float[234]?|uint[234]?|int[234]?|double)\s+($RESERVED)\b" "$SHADER"; then
  fail "an HLSL reserved word is used as an identifier; DXC reports a cascade of misleading errors"
fi
if grep -q '(void)' "$SHADER"; then
  fail "'(void)x;' is not HLSL; drop the parameter instead"
fi

if [ "$status" -eq 0 ]; then
  echo "imaginator-shader-contract: ok"
fi
exit "$status"
