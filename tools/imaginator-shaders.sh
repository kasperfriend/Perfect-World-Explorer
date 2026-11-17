#!/usr/bin/env bash
# Build the Imaginator hardware tracer's DXIL blob.
#
# Why a script instead of `build.rs`: the shader is one file that changes only
# when the tracer's output contract changes, and DXC is not a Rust dependency.
# Checking a generated binary into the repository is the smaller evil, and it
# makes the build reproducible without a DirectX shader toolchain.
#
#   tools/imaginator-shaders.sh            # build if DXC is available
#   DXC=/path/to/dxc tools/imaginator-shaders.sh
#
# The output, `imaginator/dxr_trace.cso`, is DXIL for shader model 6.5. It is the
# only form of the tracer the DXR path can execute: `RayQuery` has no WGSL or
# SPIR-V spelling, and the DX12 back end will not accept anything else.
#
# When DXC is missing, this script says so and exits 0 — the module already
# reports the blob as absent and keeps the compute tracer running, so a missing
# blob is a degraded tracer, not a broken build.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src="$root/tools/imaginator-shaders/dxr_trace.hlsl"
out="$root/imaginator/dxr_trace.cso"

find_dxc() {
    if [[ -n "${DXC:-}" ]]; then printf '%s\n' "$DXC"; return 0; fi
    if command -v dxc >/dev/null 2>&1; then command -v dxc; return 0; fi
    # The Windows SDK ships DXC under the Vulkan SDK and, more commonly for the
    # people who will run this, next to dxc.exe inside a VS install.
    for candidate in \
        "/c/Program Files (x86)/Windows Kits/10/bin/x64/dxc.exe" \
        "$HOME/VulkanSDK"/*/bin/dxc
    do
        if [[ -x "$candidate" ]]; then printf '%s\n' "$candidate"; return 0; fi
    done
    return 1
}

if ! dxc="$(find_dxc)"; then
    echo "imaginator-shaders: DXC not found; $out stays as it is." >&2
    echo "  The hardware tracer will report the blob as missing and fall back." >&2
    echo "  Install the Vulkan SDK or a Windows SDK and re-run, or set DXC=..." >&2
    exit 0
fi

mkdir -p "$(dirname "$out")"
"$dxc" \
    -T cs_6_5 \
    -E cs \
    -HV 2018 \
    -O3 \
    -Zpc \
    -all_resources_bound \
    -Fo "$out" \
    "$src"

size=$(wc -c <"$out")
if [[ "$size" -lt 64 ]]; then
    echo "imaginator-shaders: $out is only $size bytes; DXC probably failed" >&2
    exit 1
fi
echo "imaginator-shaders: wrote $out ($size bytes, cs_6_5 DXIL)"
