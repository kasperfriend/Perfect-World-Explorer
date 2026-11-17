#!/usr/bin/env bash
# Type-check and test the Imaginator's Direct3D 12 layer without Windows.
#
# The hardware tracer talks to `ID3D12Device5` and
# `ID3D12GraphicsCommandList4`, interfaces `winapi` 0.3.9 does not declare. On
# the build machine those come from the real `d3d12` crate and the real
# `winapi`; here they come from the minimal stand-ins in
# `tools/imaginator-check/`, which mirror only the members the module calls.
#
# What this proves:
#
#   * the vtable slot arithmetic in `src/imaginator/hardware/sys.rs` (compiled
#     and pinned with `const _: () = assert!`s);
#   * the descriptor layouts against `d3d12.h`'s documented sizes;
#   * that `hardware/mod.rs` type-checks: every COM call, every barrier, every
#     buffer it creates;
#   * its own `#[cfg(test)]` tests run.
#
# What it cannot prove: that the driver accepts the calls, and that the DXIL blob
# loads. Both need Windows hardware.
#
#   tools/imaginator-check.sh          the hardware layer: check + test
#   tools/imaginator-check.sh check    the hardware layer: type-check only
#   tools/imaginator-check.sh capture  capture.rs against stand-in engine types
#   tools/imaginator-check.sh module   the whole module (needs egui + bytemuck shims)
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$REPO/target/offline"
HERE="$REPO/tools/imaginator-check"
TRIPLE=x86_64-unknown-linux-gnu
RUSTC_VERSION=1.88.0
RUSTC="$OUT/rt/node_modules/@rustbin/rustc-$RUSTC_VERSION-$TRIPLE/rustc/bin/rustc"

if [ ! -x "$RUSTC" ]; then
    echo "imaginator-check: no rustc at $RUSTC; run tools/offline_harness.sh setup" >&2
    exit 1
fi
export LD_LIBRARY_PATH="$OUT/rt/node_modules/@rustbin/rustc-$RUSTC_VERSION-$TRIPLE/rustc/lib"
CFLAGS=(--edition 2021 --sysroot "$OUT/sysroot" -A non_snake_case)

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

"$RUSTC" "${CFLAGS[@]}" --crate-type lib --crate-name winapi \
    -o "$WORK/libwinapi.rlib" "$HERE/winapi.rs"
"$RUSTC" "${CFLAGS[@]}" --crate-type lib --crate-name d3d12 \
    -o "$WORK/libd3d12.rlib" --extern "winapi=$WORK/libwinapi.rlib" "$HERE/d3d12.rs"
# `hardware/mod.rs` uses `crate::fsr::raw_dx12`; the module check shims it. The
# hardware-only check mounts `hardware/mod.rs` at the root, where `super::sys`
# works, so both are covered by their own mode.

"$RUSTC" "${CFLAGS[@]}" --crate-type lib --crate-name wgpu \
    -o "$WORK/libwgpu.rlib" "$HERE/wgpu.rs"
"$RUSTC" "${CFLAGS[@]}" --crate-type lib --crate-name bytemuck \
    -o "$WORK/libbytemuck.rlib" "$HERE/bytemuck.rs"
"$RUSTC" "${CFLAGS[@]}" --crate-type lib --crate-name egui \
    -o "$WORK/libegui.rlib" "$HERE/egui.rs"

mode="${1:-test}"
if [ "$mode" = module ]; then
    # The whole module: root.rs supplies wgpu/fsr/d3d12 shims, the real bvh,
    # math, settings, tracer, gpu, capture, composite and hardware files are
    # mounted under it. `egui` is the one extern it needs.
    "$RUSTC" "${CFLAGS[@]}" --test -o "$WORK/module.bin" \
        --extern "winapi=$WORK/libwinapi.rlib" \
        --extern "d3d12_crate=$WORK/libd3d12.rlib" \
        -L "$WORK" \
        --extern "wgpu=$WORK/libwgpu.rlib" \
        --extern "egui=$WORK/libegui.rlib" \
        --extern "bytemuck=$WORK/libbytemuck.rlib" \
        "$HERE/module_root.rs"
    "$WORK/module.bin"
    exit $?
fi
if [ "$mode" = capture ]; then
    # `capture.rs` reads engine types (MapSpace, WaterArea, bmd::Model, Trees);
    # this root supplies stand-ins with the same fields and the real bvh/math.
    "$RUSTC" "${CFLAGS[@]}" --test -o "$WORK/capture.bin" "$HERE/capture_root.rs"
    "$WORK/capture.bin"
    exit $?
fi
args=(
    --extern "winapi=$WORK/libwinapi.rlib"
    --extern "d3d12=$WORK/libd3d12.rlib"
    --extern "wgpu=$WORK/libwgpu.rlib"
)
if [ "$mode" = check ]; then
    "$RUSTC" "${CFLAGS[@]}" --crate-type lib -o "$WORK/check.rlib" \
        "${args[@]}" "$HERE/root.rs"
    echo "imaginator-check: hardware layer type-checks"
else
    "$RUSTC" "${CFLAGS[@]}" --test -o "$WORK/check.bin" \
        "${args[@]}" "$HERE/root.rs"
    "$WORK/check.bin"
fi
