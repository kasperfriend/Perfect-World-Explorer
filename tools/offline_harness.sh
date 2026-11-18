#!/usr/bin/env bash
# Offline compiler and test runner for the gfx runtime modules.
#
# crates.io is unreachable from this workspace, so `cargo` cannot resolve the
# dependency graph and the only full build normally available is CI, at roughly
# five minutes a cycle. This script assembles a compiler from parts that *are*
# reachable -- npm for the Rust toolchain, GitHub for the two crates the runtime
# actually needs -- and type-checks and runs the twelve modules whose logic needs
# no GPU in seconds: `animation`, `client_weather`, `ecmodel`, `gfx_color_track`,
# `gfx_decal`, `gfx_file`, `gfx_light`, `gfx_owner`, `gfx_player`, `scene`,
# `scene_lights`, `sky_transition` and `view_settings`. That is the ECWLD
# record layout under test as well as the effect runtime. What
# stays CI-only is anything that touches wgpu, egui or winit: the renderer, the
# world, the effect draw object and the walker.
#
#   tools/offline_harness.sh setup    install toolchain and build dependencies
#   tools/offline_harness.sh check    type-check the closure (no test code)
#   tools/offline_harness.sh test     build and run every #[cfg(test)] in it
#   tools/offline_harness.sh fmt      rustfmt --check the modules CI formats
#
# Everything lands in target/offline/, which .gitignore already excludes.
#
# Corrections to what this script used to claim (2026-11-18). The real flate2
# *can* be built offline: clone flate2 1.0.35, miniz_oxide 0.8.4 (it lives in
# the `miniz/miniz_oxide` subdirectory of that repository), crc32fast, adler2
# and cfg-if, then build them in dependency order with
#
#   rustc --crate-type rlib -C opt-level=1 --sysroot <sysroot> \
#         [--cfg 'feature="…"'] -L target/offline -o libcrate.rlib src/lib.rs
#
# (miniz_oxide needs `--cfg feature="with-alloc"`; flate2 needs
# `--cfg feature="any_impl"` and `--cfg feature="rust_backend"` plus
# `--cfg feature="miniz_oxide"`). The earlier E0463/E0460 failures came from
# omitting `-L` for flate2's own dependencies, not from the sysroot. Once
# target/offline/libflate2.rlib exists, pass
# `--extern flate2=…/libflate2.rlib` and the real `crate::pck` is tested
# instead of the stub.
#
# What this still does not do: use the real encoding_rs, whose default build
# pulls multiversion, simdutf8 and core_detect. The GBK decoder here is
# generated from Python's `gbk` codec instead, which covers every double-byte
# sequence the client's text files use.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$REPO/target/offline"
RT="$OUT/rt"
SYSROOT="$OUT/sysroot"
DEP="$OUT/dep"
TRIPLE=x86_64-unknown-linux-gnu
RUSTC_VERSION=1.88.0

mkdir -p "$OUT"

toolchain() {
    if [ ! -d "$RT/node_modules/@rustbin" ]; then
        mkdir -p "$RT"
        (cd "$RT" && npm install --silent --no-audit --no-fund \
            "@rustbin/rustc-$RUSTC_VERSION-$TRIPLE@$RUSTC_VERSION" \
            "@rustbin/rust-std-$RUSTC_VERSION-$TRIPLE@$RUSTC_VERSION" \
            "@rustbin/rustfmt-$RUSTC_VERSION-$TRIPLE@1.8.0")
    fi
    # The rustc package nests its payload under an extra rustc/ directory.
    RUSTC="$RT/node_modules/@rustbin/rustc-$RUSTC_VERSION-$TRIPLE/rustc/bin/rustc"
    export LD_LIBRARY_PATH="$RT/node_modules/@rustbin/rustc-$RUSTC_VERSION-$TRIPLE/rustc/lib"
    PATH="$RT/node_modules/@rustbin/rustfmt-$RUSTC_VERSION-$TRIPLE/rustfmt-preview/bin:$PATH"
    export PATH
    [ -x "$RUSTC" ] || { echo "rustc missing; run: $0 setup" >&2; exit 1; }
}

sysroot() {
    if [ ! -f "$SYSROOT/lib/rustlib/$TRIPLE/lib/libstd-"*.rlib ] 2>/dev/null ||
        ! ls "$SYSROOT/lib/rustlib/$TRIPLE/lib/"libstd-*.rlib >/dev/null 2>&1; then
        mkdir -p "$SYSROOT/lib/rustlib/$TRIPLE/lib"
        cp "$RT/node_modules/@rustbin/rustc-$RUSTC_VERSION-$TRIPLE/rustc/lib/"*.so "$SYSROOT/lib/"
        cp "$RT/node_modules/@rustbin/rust-std-$RUSTC_VERSION-$TRIPLE/rust-std-$TRIPLE/lib/rustlib/$TRIPLE/lib/"*.rlib \
            "$SYSROOT/lib/rustlib/$TRIPLE/lib/"
    fi
}

gbk_table() {
    [ -f "$OUT/gbk_table.rs" ] && return 0
    python3 - "$OUT/gbk_table.rs" <<'PY'
import sys
rows, ok = [], 0
for lead in range(0x81, 0xFF):
    for trail in range(0x40, 0xFF):
        if trail == 0x7F:
            rows.append(0); continue
        try:
            ch = bytes([lead, trail]).decode('gbk')
        except Exception:
            rows.append(0); continue
        if len(ch) == 1:
            rows.append(ord(ch)); ok += 1
        else:
            rows.append(0)
with open(sys.argv[1], 'w') as f:
    f.write("// Generated from Python's gbk codec; %d mapped sequences.\n" % ok)
    f.write("pub const TABLE: [u32; %d] = [\n" % len(rows))
    line = []
    for v in rows:
        line.append(str(v))
        if len(line) == 20:
            f.write(",".join(line) + ",\n"); line = []
    if line:
        f.write(",".join(line) + ",\n")
    f.write("];\n")
PY
}

deps() {
    gbk_table
    if [ ! -d "$DEP/glam" ]; then
        mkdir -p "$DEP"
        # glam's tags carry no `v` prefix.
        git clone -q --depth 1 --branch 0.27.0 https://github.com/bitshifter/glam-rs "$DEP/glam"
    fi
    if [ ! -f "$OUT/libglam.rlib" ]; then
        # rustc does not apply Cargo's default features, so `std` must be passed
        # by hand or glam builds as no_std and every f32 intrinsic disappears.
        "$RUSTC" --crate-type rlib --crate-name glam --edition 2021 --sysroot "$SYSROOT" \
            --cfg 'feature="std"' -C opt-level=1 "$DEP/glam/src/lib.rs" -o "$OUT/libglam.rlib"
    fi
    if [ ! -f "$OUT/libencoding_rs.rlib" ]; then
        cat > "$OUT/gbk_shim.rs" <<EOF
//! Local stand-in for \`encoding_rs\`, harness only; the shipped build links the
//! real crate. WHATWG GBK, from a table generated by Python's \`gbk\` codec.
use std::borrow::Cow;

mod table {
    include!("$OUT/gbk_table.rs");
}

pub struct Encoding {
    pub name: &'static str,
}

pub static GBK: Encoding = Encoding { name: "GBK" };

impl Encoding {
    pub fn decode<'a>(&self, bytes: &'a [u8]) -> (Cow<'a, str>, &'static Encoding, bool) {
        if bytes.is_ascii() {
            let text = std::str::from_utf8(bytes).expect("ascii is utf-8");
            return (Cow::Borrowed(text), &GBK, false);
        }
        let mut out = String::with_capacity(bytes.len());
        let mut errors = false;
        let mut i = 0;
        while i < bytes.len() {
            let lead = bytes[i];
            if lead < 0x80 {
                out.push(lead as char);
                i += 1;
                continue;
            }
            if lead == 0x80 {
                out.push('\u{20ac}');
                i += 1;
                continue;
            }
            if i + 1 < bytes.len() {
                let trail = bytes[i + 1];
                if (0x40..=0xFE).contains(&trail) && trail != 0x7F && lead <= 0xFE {
                    let cp = table::TABLE[(lead as usize - 0x81) * 191 + (trail as usize - 0x40)];
                    if cp != 0 {
                        if let Some(c) = char::from_u32(cp) {
                            out.push(c);
                            i += 2;
                            continue;
                        }
                    }
                }
            }
            errors = true;
            out.push('\u{fffd}');
            i += 1;
        }
        (Cow::Owned(out), &GBK, errors)
    }

    /// Reverse mapping, for \`ecmodel\`'s GBK encode path.
    pub fn encode<'a>(&self, src: &'a str) -> (Cow<'a, [u8]>, &'static Encoding, bool) {
        if src.is_ascii() {
            return (Cow::Borrowed(src.as_bytes()), &GBK, false);
        }
        static REV: std::sync::OnceLock<std::collections::HashMap<u32, (u8, u8)>> =
            std::sync::OnceLock::new();
        let rev = REV.get_or_init(|| {
            let mut m = std::collections::HashMap::new();
            for (i, cp) in table::TABLE.iter().enumerate() {
                if *cp != 0 {
                    m.entry(*cp)
                        .or_insert(((i / 191 + 0x81) as u8, (i % 191 + 0x40) as u8));
                }
            }
            m
        });
        let mut out = Vec::with_capacity(src.len() * 2);
        let mut unmappable = false;
        for c in src.chars() {
            let cp = c as u32;
            if cp < 0x80 {
                out.push(cp as u8);
            } else if cp == 0x20ac {
                out.push(0x80);
            } else {
                match rev.get(&cp) {
                    Some(&(lead, trail)) => {
                        out.push(lead);
                        out.push(trail);
                    }
                    None => {
                        unmappable = true;
                        out.push(b'?');
                    }
                }
            }
        }
        (Cow::Owned(out), &GBK, unmappable)
    }
}
EOF
        "$RUSTC" --crate-type rlib --crate-name encoding_rs --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=1 "$OUT/gbk_shim.rs" -o "$OUT/libencoding_rs.rlib"
    fi
    cat > "$OUT/pck_stub.rs" <<'EOF'
//! Offline stand-in for `crate::pck`; see the note at the top of the script.
use std::fs::File;

pub struct Entry {
    pub name: String,
    pub offset: u32,
    pub size: u32,
    pub compressed: u32,
}

pub fn directory(_f: &mut File) -> Result<Vec<Entry>, String> {
    Err("offline harness: no zlib backend".to_string())
}

pub fn read_entry(_f: &mut File, _e: &Entry) -> Result<Vec<u8>, String> {
    Err("offline harness: no zlib backend".to_string())
}
EOF
    cat > "$OUT/lib.rs" <<EOF
//! Local type-check/test harness mirroring the crate's module set for the
//! dependency-free closure around gfx_player. Not part of the shipped build.
#[path = "$REPO/src/animation.rs"]
pub mod animation;
#[path = "$REPO/src/client_weather.rs"]
pub mod client_weather;
#[path = "$REPO/src/ecmodel.rs"]
pub mod ecmodel;
#[path = "$REPO/src/gfx_color_track.rs"]
pub mod gfx_color_track;
#[path = "$REPO/src/gfx_decal.rs"]
pub mod gfx_decal;
#[path = "$REPO/src/gfx_file.rs"]
pub mod gfx_file;
#[path = "$REPO/src/gfx_light.rs"]
pub mod gfx_light;
#[path = "$REPO/src/gfx_owner.rs"]
pub mod gfx_owner;
#[path = "$REPO/src/gfx_player.rs"]
pub mod gfx_player;
#[path = "$REPO/src/model_material.rs"]
pub mod model_material;
#[path = "$REPO/src/skeleton_pose.rs"]
pub mod skeleton_pose;
#[path = "$REPO/src/skin_pose.rs"]
pub mod skin_pose;
#[path = "$REPO/src/model_asset.rs"]
pub mod model_asset;
#[path = "$REPO/src/skeleton.rs"]
pub mod skeleton;
#[path = "$REPO/src/skin.rs"]
pub mod skin;
#[path = "$REPO/src/model_dds.rs"]
pub mod model_dds;
#[path = "$REPO/src/scene.rs"]
pub mod scene;
#[path = "$REPO/src/scene_lights.rs"]
pub mod scene_lights;
#[path = "$REPO/src/sky_transition.rs"]
pub mod sky_transition;
#[path = "$REPO/src/view_settings.rs"]
pub mod view_settings;
#[path = "$OUT/pck_stub.rs"]
pub mod pck;
EOF
}

EXT="--extern glam=$OUT/libglam.rlib --extern encoding_rs=$OUT/libencoding_rs.rlib"

case "${1:-test}" in
    setup) toolchain; sysroot; deps; echo "harness ready in $OUT" ;;
    check) toolchain; sysroot; deps
        "$RUSTC" --crate-type lib --crate-name harness --edition 2021 --sysroot "$SYSROOT" \
            $EXT -C opt-level=0 "$OUT/lib.rs" -o "$OUT/libharness.rlib"
        echo "type-check clean" ;;
    test)  toolchain; sysroot; deps
        "$RUSTC" --test --crate-name harness --edition 2021 --sysroot "$SYSROOT" \
            $EXT -C opt-level=1 "$OUT/lib.rs" -o "$OUT/gfxtest"
        shift || true
        (cd "$REPO" && exec "$OUT/gfxtest" "$@") ;;
    fmt)   toolchain
        rustfmt --check --edition 2021 \
            "$REPO/src/gfx_player.rs" "$REPO/src/gfx_file.rs" "$REPO/src/gfx_decal.rs" \
            "$REPO/src/gfx_color_track.rs" "$REPO/src/gfx_catalog.rs" "$REPO/src/ecmodel.rs" \
            "$REPO/src/gfx_light.rs" "$REPO/src/gfx_owner.rs" "$REPO/src/scene_lights.rs" \
            "$REPO/src/view_settings.rs" "$REPO/src/client_weather.rs" \
            "$REPO/src/sky_transition.rs" "$REPO/src/scene.rs" "$REPO/src/model_dds.rs" "$REPO/src/skin.rs" "$REPO/src/skeleton.rs" "$REPO/src/skeleton_pose.rs" "$REPO/src/skin_pose.rs" "$REPO/src/model_asset.rs" "$REPO/src/model_material.rs" ;;
    *) echo "usage: $0 {setup|check|test|fmt}" >&2; exit 2 ;;
esac
