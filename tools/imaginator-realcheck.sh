#!/usr/bin/env bash
# Type-check the Direct3D 12 layer against the *real* `winapi` 0.3.9 and the
# *real* `d3d12` 0.19 sources, not the stand-ins in `tools/imaginator-check/`.
#
# Why this exists
# ---------------
# `tools/imaginator-check.sh` compiles `hardware/dx12.rs` against hand-written
# shims. Shims are only as strict as whoever wrote them, and four Windows-only
# build failures came from exactly that gap:
#
#   * `uuidof()` was declared inherent instead of coming from `winapi::Interface`,
#     so a missing `use winapi::Interface` passed offline and failed on Windows;
#   * the raw COM methods were declared safe, hiding three missing `unsafe`
#     blocks (E0133) in `CheckFeatureSupport`, `CreateCommittedResource` and
#     `GetCompletedValue`;
#   * `D3D12_RESOURCE_BARRIER_u::Transition_mut` was declared safe, hiding the
#     same error in the barrier helper;
#   * `DXGI_FORMAT_UNKNOWN`/`DXGI_SAMPLE_DESC` were re-exported from the `d3d12`
#     module, where the real crate only imports them privately.
#
# This script removes the stand-ins from the picture: it compiles the actual
# crates the Windows job downloads, then type-checks `dx12.rs` against them. It
# still cannot *run* Direct3D 12 (no Windows hardware here), but every name,
# signature, `unsafe` requirement and feature gate now comes from the real crate.
#
#   tools/imaginator-realcheck.sh           the hardware layer, then the whole
#   tools/imaginator-realcheck.sh hardware  the hardware layer only
#   tools/imaginator-realcheck.sh module    the whole module, Windows code path
#
# Needs `tools/offline_harness.sh setup` (for rustc) and network access to
# codeload.github.com; sources are cached under `target/offline/real/`.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$REPO/target/offline"
CACHE="$OUT/real"
TRIPLE=x86_64-unknown-linux-gnu
RUSTC_VERSION=1.88.0
RUSTC="$OUT/rt/node_modules/@rustbin/rustc-$RUSTC_VERSION-$TRIPLE/rustc/bin/rustc"

WINAPI_VERSION=0.3.9
BITFLAGS_VERSION=2.4.1
# `gfx-rs/d3d12-rs` master at the 0.19.0 release; the crate has no `0.19.0` tag.
D3D12_COMMIT=00a32bd0b4b4fd68759b17bd6f00052d5af79967

if [ ! -x "$RUSTC" ]; then
    echo "imaginator-realcheck: no rustc at $RUSTC; run tools/offline_harness.sh setup" >&2
    exit 1
fi
export LD_LIBRARY_PATH="$OUT/rt/node_modules/@rustbin/rustc-$RUSTC_VERSION-$TRIPLE/rustc/lib"
CFLAGS=(--sysroot "$OUT/sysroot" -A non_snake_case --cap-lints allow)
mkdir -p "$CACHE"

fetch() { # fetch <url> <directory> [<name>]
    local url="$1" directory="$2" name="$3"
    if [ ! -d "$directory/$name" ]; then
        echo "imaginator-realcheck: fetching $name" >&2
        curl -sSL "$url" -o "$directory/$name.tar.gz"
        tar xzf "$directory/$name.tar.gz" -C "$directory"
    fi
}

# 1. `winapi` 0.3.9. It is edition 2015 and `#![cfg(windows)]`, so it needs
#    `--cfg windows`; that flag is a rustc lint now, allowed explicitly rather
#    than silently. Every feature is on, which is a superset of the set
#    `Cargo.toml` and `d3d12`'s manifest ask for — `cfg`-gated modules reference
#    each other freely, so a subset build of this crate does not compile at all.
fetch "https://codeload.github.com/retep998/winapi-rs/tar.gz/refs/tags/$WINAPI_VERSION" \
    "$CACHE" "winapi-rs-$WINAPI_VERSION"
mapfile -t CFGS < <(sed -n '/^\[features\]/,/^\[/p' \
    "$CACHE/winapi-rs-$WINAPI_VERSION/Cargo.toml" \
    | grep -oE '^"?[a-z0-9_]+"?' | tr -d '"' | grep -vx -e features -e std \
    | sed 's/.*/--cfg=feature="&"/')
WINAPI_CFLAGS=(--edition 2015 --crate-type lib --crate-name winapi
    --cfg windows --cfg 'feature="std"' "${CFGS[@]}"
    --allow explicit_builtin_cfgs_in_flags)
"$RUSTC" "${CFLAGS[@]}" "${WINAPI_CFLAGS[@]}" \
    -o "$CACHE/libwinapi_real.rlib" "$CACHE/winapi-rs-$WINAPI_VERSION/src/lib.rs"

# 2. `bitflags` 2, a `d3d12` dependency.
fetch "https://codeload.github.com/bitflags/bitflags/tar.gz/refs/tags/$BITFLAGS_VERSION" \
    "$CACHE" "bitflags-$BITFLAGS_VERSION"
"$RUSTC" "${CFLAGS[@]}" --edition 2021 --crate-type lib --crate-name bitflags \
    -o "$CACHE/libbitflags.rlib" "$CACHE/bitflags-$BITFLAGS_VERSION/src/lib.rs"

# 3. `d3d12` itself, at the commit `cargo` resolves 0.19.0 to.
fetch "https://codeload.github.com/gfx-rs/d3d12-rs/tar.gz/$D3D12_COMMIT" \
    "$CACHE" "d3d12-rs-$D3D12_COMMIT"
"$RUSTC" "${CFLAGS[@]}" --edition 2018 --crate-type lib --crate-name d3d12 \
    --cfg windows --allow explicit_builtin_cfgs_in_flags \
    --extern winapi="$CACHE/libwinapi_real.rlib" \
    --extern bitflags="$CACHE/libbitflags.rlib" \
    -L "$CACHE" -o "$CACHE/libd3d12_real.rlib" \
    "$CACHE/d3d12-rs-$D3D12_COMMIT/src/lib.rs"

# 4. The code under test against those. Only `wgpu`/`egui`/`bytemuck` remain
#    shimmed, because they are not what is being checked.
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
for shim in wgpu egui bytemuck; do
    "$RUSTC" "${CFLAGS[@]}" --edition 2021 --crate-type lib --crate-name "$shim" \
        -o "$WORK/lib$shim.rlib" "$REPO/tools/imaginator-check/$shim.rs"
done
EXTERNS=(
    --extern winapi="$CACHE/libwinapi_real.rlib"
    --extern d3d12="$CACHE/libd3d12_real.rlib"
    --extern d3d12_crate="$CACHE/libd3d12_real.rlib"
    --extern wgpu="$WORK/libwgpu.rlib"
    --extern egui="$WORK/libegui.rlib"
    --extern bytemuck="$WORK/libbytemuck.rlib"
    -L "$CACHE" -L "$WORK"
)
WINDOWS=(--cfg windows --allow explicit_builtin_cfgs_in_flags)

mode="${1:-all}"

# `hardware`: `root.rs` mounts `dx12.rs` at the crate root exactly as the Windows
# build mounts it, so `super::sys` resolves.
if [ "$mode" = all ] || [ "$mode" = hardware ]; then
    "$RUSTC" "${CFLAGS[@]}" --edition 2021 --crate-type lib "${WINDOWS[@]}" \
        "${EXTERNS[@]}" -o "$WORK/check.rlib" "$REPO/tools/imaginator-check/root.rs"
    echo "imaginator-realcheck: hardware layer type-checks against winapi $WINAPI_VERSION + d3d12 0.19"
fi

# `module`: the whole Imaginator compiled down the `#[cfg(windows)]` path, which
# the offline `imaginator-check.sh module` never takes. This is the closest thing
# to the Windows viewer build that can run here.
if [ "$mode" = all ] || [ "$mode" = module ]; then
    sed "s#\"\\.\\./\\.\\./src/#\"$REPO/src/#" \
        "$REPO/tools/imaginator-check/module_root.rs" > "$WORK/module_root.rs"
    "$RUSTC" "${CFLAGS[@]}" --edition 2021 --test "${WINDOWS[@]}" \
        "${EXTERNS[@]}" -o "$WORK/module.bin" "$WORK/module_root.rs"
    "$WORK/module.bin"
fi
