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
#   tools/offline_harness.sh client   build+test the client_detect closure
#                                     against the REAL goblin/flate2/sha2
#   tools/offline_harness.sh water    build+test `src/water.rs` against the REAL
#                                     flate2 (BSData records are zlib-framed)
#   tools/offline_harness.sh gfx      build+test the GFX containers (file, decal,
#                                     light) and the `pck` they read from
#   tools/offline_harness.sh music    build+test `src/music.rs` (precinct zones and
#                                     the teleport fallback points)
#   tools/offline_harness.sh material build+test the terrain material set and the
#                                     missing-texture placeholder the client paints#
# Everything lands in target/offline/, which .gitignore already excludes.
#
# The real `flate2`, `goblin` and a correct SHA-256 are built here from pinned
# sources with plain `rustc`; only `encoding_rs` is a stand-in (its default
# build pulls multiversion/simdutf8/core_detect, and the GBK table generated
# from Python's `gbk` codec covers every double-byte sequence the client's text
# files use).
#
# Earlier revisions of this script used stand-ins for `goblin` and `sha2` too,
# and claimed flate2 could not be built offline. Both were wrong, and the
# consequence was real: the `goblin` shim invented a `resource_directory` field
# that goblin 0.8.2 does not have, so `cargo check --bins` in CI was the first
# thing to notice. Build recipes worth remembering:
#
#   rustc --crate-type rlib -C opt-level=1 --sysroot <sysroot> -L <out> \
#         [--cfg 'feature="…"'] -o libcrate.rlib <crate>/src/lib.rs
#
#   miniz_oxide  --cfg feature="with-alloc"      (it lives in miniz_oxide/ of
#                                                 the miniz/miniz_oxide clone)
#   flate2       --cfg feature="any_impl" --cfg feature="rust_backend"
#                --cfg feature="miniz_oxide"
#   scroll       --cfg feature="std" --cfg feature="derive"  (needs the
#                proc-macro chain: unicode_ident, proc_macro2, quote, syn)
#   goblin       --cfg feature="std"/"alloc"/"endian_fd"/"pe32"/"pe64"/"archive"
#
# `-L <out>` is required for a crate to find its own dependencies; the original
# E0463/E0460 failures came from omitting it, not from the sysroot.
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

# The real `goblin`, `flate2` and `sha2`, built from source with rustc (there is
# no Cargo here). Stand-ins are what let a `client_detect` type error through to
# CI: the first version of the offline shim invented a `resource_directory`
# field that goblin 0.8.2 does not have, and only `cargo check --bins` noticed.
# Mirroring the real crates is therefore the point of this function.
real_deps() {
    if [ ! -d "$DEP/goblin" ]; then
        mkdir -p "$DEP"
        git clone -q --depth 1 --branch 0.8.2 https://github.com/m4b/goblin "$DEP/goblin"
        git clone -q --depth 1 https://github.com/m4b/scroll "$DEP/scroll"
        git clone -q --depth 1 https://github.com/rust-lang/log "$DEP/log"
        git clone -q --depth 1 https://github.com/dtolnay/syn "$DEP/syn"
        git clone -q --depth 1 https://github.com/dtolnay/quote "$DEP/quote"
        git clone -q --depth 1 https://github.com/dtolnay/proc-macro2 "$DEP/proc_macro2"
        git clone -q --depth 1 https://github.com/dtolnay/unicode-ident "$DEP/unicode_ident"
    fi
    # proc-macro chain, needed because goblin's `scroll` derives with them.
    [ -f "$OUT/libunicode_ident.rlib" ] || "$RUSTC" --crate-type rlib \
        --crate-name unicode_ident --edition 2021 --sysroot "$SYSROOT" -C opt-level=1 \
        "$DEP/unicode_ident/src/lib.rs" -o "$OUT/libunicode_ident.rlib"
    [ -f "$OUT/libproc_macro2.rlib" ] || "$RUSTC" --crate-type rlib \
        --crate-name proc_macro2 --edition 2021 --sysroot "$SYSROOT" -C opt-level=1 \
        --cfg 'feature="proc-macro"' --extern unicode_ident="$OUT/libunicode_ident.rlib" \
        "$DEP/proc_macro2/src/lib.rs" -o "$OUT/libproc_macro2.rlib"
    [ -f "$OUT/libquote.rlib" ] || "$RUSTC" --crate-type rlib \
        --crate-name quote --edition 2021 --sysroot "$SYSROOT" -C opt-level=1 \
        --cfg 'feature="proc-macro"' -L "$OUT" \
        --extern proc_macro2="$OUT/libproc_macro2.rlib" \
        "$DEP/quote/src/lib.rs" -o "$OUT/libquote.rlib"
    [ -f "$OUT/libsyn.rlib" ] || "$RUSTC" --crate-type rlib \
        --crate-name syn --edition 2021 --sysroot "$SYSROOT" -C opt-level=1 \
        --cfg 'feature="derive"' --cfg 'feature="parsing"' --cfg 'feature="printing"' \
        --cfg 'feature="clone-impls"' --cfg 'feature="proc-macro"' -L "$OUT" \
        --extern proc_macro2="$OUT/libproc_macro2.rlib" --extern quote="$OUT/libquote.rlib" \
        --extern unicode_ident="$OUT/libunicode_ident.rlib" \
        "$DEP/syn/src/lib.rs" -o "$OUT/libsyn.rlib"
    [ -f "$OUT/libscroll_derive.so" ] || "$RUSTC" --crate-type proc-macro \
        --crate-name scroll_derive --edition 2021 --sysroot "$SYSROOT" -C opt-level=1 \
        -L "$OUT" --extern proc_macro2="$OUT/libproc_macro2.rlib" \
        --extern quote="$OUT/libquote.rlib" --extern syn="$OUT/libsyn.rlib" \
        "$DEP/scroll/scroll_derive/src/lib.rs" -o "$OUT/libscroll_derive.so"
    [ -f "$OUT/libscroll.rlib" ] || "$RUSTC" --crate-type rlib --crate-name scroll \
        --edition 2021 --sysroot "$SYSROOT" -C opt-level=1 \
        --cfg 'feature="std"' --cfg 'feature="derive"' -L "$OUT" \
        --extern scroll_derive="$OUT/libscroll_derive.so" \
        "$DEP/scroll/src/lib.rs" -o "$OUT/libscroll.rlib"
    [ -f "$OUT/liblog.rlib" ] || "$RUSTC" --crate-type rlib --crate-name log \
        --edition 2021 --sysroot "$SYSROOT" -C opt-level=1 --cfg 'feature="std"' \
        "$DEP/log/src/lib.rs" -o "$OUT/liblog.rlib"
    [ -f "$OUT/libgoblin.rlib" ] || "$RUSTC" --crate-type rlib --crate-name goblin \
        --edition 2021 --sysroot "$SYSROOT" -C opt-level=1 \
        --cfg 'feature="std"' --cfg 'feature="alloc"' --cfg 'feature="endian_fd"' \
        --cfg 'feature="pe32"' --cfg 'feature="pe64"' --cfg 'feature="archive"' \
        -L "$OUT" --extern scroll="$OUT/libscroll.rlib" --extern log="$OUT/liblog.rlib" \
        "$DEP/goblin/src/lib.rs" -o "$OUT/libgoblin.rlib"
    # `flate2` is what `pck` and `angelica_pck` actually use, and the water
    # BSData reader needs it too (`stored, raw, zlib` records). It was claimed
    # below but never built, so every `client` run failed on a missing extern.
    if [ ! -f "$OUT/libflate2.rlib" ]; then
        if [ ! -d "$DEP/flate2" ]; then
            mkdir -p "$DEP"
            git clone -q --depth 1 https://github.com/rust-lang/flate2-rs "$DEP/flate2"
            git clone -q --depth 1 https://github.com/Frommi/miniz_oxide "$DEP/miniz_oxide"
            git clone -q --depth 1 https://github.com/oyvindln/adler2 "$DEP/adler2"
            git clone -q --depth 1 https://github.com/srijs/rust-crc32fast "$DEP/crc32fast"
            git clone -q --depth 1 https://github.com/rust-lang/cfg-if "$DEP/cfg_if"
        fi
        [ -f "$OUT/libadler2.rlib" ] || "$RUSTC" --crate-type rlib \
            --crate-name adler2 --edition 2021 --sysroot "$SYSROOT" -C opt-level=1 \
            "$DEP/adler2/src/lib.rs" -o "$OUT/libadler2.rlib"
        [ -f "$OUT/libminiz_oxide.rlib" ] || "$RUSTC" --crate-type rlib \
            --crate-name miniz_oxide --edition 2021 --sysroot "$SYSROOT" -C opt-level=1 \
            --cfg 'feature="with-alloc"' -L "$OUT" --extern adler2="$OUT/libadler2.rlib" \
            "$DEP/miniz_oxide/miniz_oxide/src/lib.rs" -o "$OUT/libminiz_oxide.rlib"
        [ -f "$OUT/libcfg_if.rlib" ] || "$RUSTC" --crate-type rlib \
            --crate-name cfg_if --edition 2021 --sysroot "$SYSROOT" -C opt-level=1 \
            "$DEP/cfg_if/src/lib.rs" -o "$OUT/libcfg_if.rlib"
        [ -f "$OUT/libcrc32fast.rlib" ] || "$RUSTC" --crate-type rlib \
            --crate-name crc32fast --edition 2021 --sysroot "$SYSROOT" -C opt-level=1 \
            --cfg 'feature="std"' -L "$OUT" --extern cfg_if="$OUT/libcfg_if.rlib" \
            "$DEP/crc32fast/src/lib.rs" -o "$OUT/libcrc32fast.rlib"
        "$RUSTC" --crate-type rlib --crate-name flate2 --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=1 --cfg 'feature="any_impl"' --cfg 'feature="rust_backend"' \
            --cfg 'feature="miniz_oxide"' -L "$OUT" \
            --extern miniz_oxide="$OUT/libminiz_oxide.rlib" \
            --extern crc32fast="$OUT/libcrc32fast.rlib" \
            "$DEP/flate2/src/lib.rs" -o "$OUT/libflate2.rlib"
    fi    # `sha2` compresses through `digest`/`generic-array`, a lot of transitive
    # surface for one known-answer test; the stand-in is a correct SHA-256
    # instead (below), so `hash_reference` stays meaningful.
    if [ ! -f "$OUT/libsha2.rlib" ]; then
        cat > "$OUT/sha256_shim.rs" <<'EOF'
//! `sha2`'s surface for `client_detect`, with a correct SHA-256 underneath, so
//! the known-answer test is a real check. The shipped build links the crate.
use std::fmt;

pub trait Digest {
    type Output: fmt::LowerHex;
    fn digest(data: &[u8]) -> Self::Output;
}

pub struct Sha256;

pub struct Output([u8; 32]);

impl fmt::LowerHex for Output {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Digest for Sha256 {
    type Output = Output;
    fn digest(data: &[u8]) -> Output {
        let mut state: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let mut message = data.to_vec();
        let bits = (data.len() as u64) * 8;
        message.push(0x80);
        while message.len() % 64 != 56 {
            message.push(0);
        }
        message.extend_from_slice(&bits.to_be_bytes());
        for block in message.chunks_exact(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([
                    block[i * 4],
                    block[i * 4 + 1],
                    block[i * 4 + 2],
                    block[i * 4 + 3],
                ]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let mut v = state;
            for i in 0..64 {
                let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
                let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
                let t1 = v[7]
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
                let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
                let t2 = s0.wrapping_add(maj);
                v[7] = v[6];
                v[6] = v[5];
                v[5] = v[4];
                v[4] = v[3].wrapping_add(t1);
                v[3] = v[2];
                v[2] = v[1];
                v[1] = v[0];
                v[0] = t1.wrapping_add(t2);
            }
            for i in 0..8 {
                state[i] = state[i].wrapping_add(v[i]);
            }
        }
        let mut out = [0u8; 32];
        for (i, word) in state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        Output(out)
    }
}
EOF
        "$RUSTC" --crate-type rlib --crate-name sha2 --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=1 "$OUT/sha256_shim.rs" -o "$OUT/libsha2.rlib"
    fi
    # The client-detection closure: `client_detect` calls the real `pck` and
    # `angelica_pck`, so this suite now tests them instead of a stub.
    mkdir -p "$OUT/cd"
    cat > "$OUT/cd/harness.rs" <<EOF
#[path = "$REPO/src/client_detect.rs"]
pub mod client_detect;
#[path = "$REPO/src/pck.rs"]
pub mod pck;
#[path = "$REPO/src/angelica_pck.rs"]
pub mod angelica_pck;
EOF
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
    real_deps
    cat > "$OUT/pck_stub.rs" <<'EOF'
//! Offline stand-in for `crate::pck`; see the note at the top of the script.
use std::fs::File;

pub(crate) fn inflate(_raw: &[u8], _expected: usize) -> Result<Vec<u8>, String> {
    Err("offline harness: no zlib backend".to_string())
}

pub fn corpus_absent(_what: &str) {}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    client) toolchain; sysroot; deps
        shift || true
        "$RUSTC" --test --crate-name cdtest --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=1 -L "$OUT" \
            --extern flate2="$OUT/libflate2.rlib" \
            --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            --extern sha2="$OUT/libsha2.rlib" \
            --extern goblin="$OUT/libgoblin.rlib" \
            "$OUT/cd/harness.rs" -o "$OUT/cdtest"
        (cd "$REPO" && exec "$OUT/cdtest" "$@") ;;
    water) toolchain; sysroot; deps
        # `water` needs flate2 (BSData records are zlib-framed) and the parse
        # path does not touch `image`, so a small shim stands in for it.
        real_deps
        mkdir -p "$OUT/water"
        cat > "$OUT/water/lib.rs" <<EOF
pub mod scene {
    pub struct WaterRef { pub export_id: u32, pub offset: u32 }
    pub struct Block { pub water: Vec<WaterRef> }
    pub struct Scene { pub blocks: Vec<Block> }
}
#[path = "$OUT/water/pck.rs"]
pub mod pck;
#[path = "$REPO/src/water.rs"]
pub mod water;
EOF
        cat > "$OUT/water/pck.rs" <<'EOF'
//! `inflate` from the shipped `pck`, so record framing is tested for real.
pub(crate) fn inflate(raw: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    use flate2::{Decompress, FlushDecompress, Status};
    let mut decomp = Decompress::new(true);
    let cap = expected.checked_add(1).ok_or("zlib size overflow")?;
    let mut out = Vec::with_capacity(cap);
    let status = decomp
        .decompress_vec(raw, &mut out, FlushDecompress::Finish)
        .map_err(|e| e.to_string())?;
    if out.len() != expected {
        return Err(format!("bad decompressed size: {} != {expected}", out.len()));
    }
    if status != Status::StreamEnd {
        return Err("truncated zlib stream".into());
    }
    Ok(out)
}
pub struct Entry { pub name: String, pub offset: u32, pub size: u32, pub compressed: u32 }
pub fn directory(_f: &mut std::fs::File) -> Result<Vec<Entry>, String> { Err("stub".into()) }
pub fn read_entry(_f: &mut std::fs::File, _e: &Entry) -> Result<Vec<u8>, String> { Err("stub".into()) }
EOF
        cat > "$OUT/water/image.rs" <<'EOF'
//! The few `image` items `src/water.rs` names outside the decode path.
pub struct RgbaImage { pub w: u32, pub h: u32, pub pixels: Vec<u8> }
impl RgbaImage {
    pub fn from_raw(w: u32, h: u32, pixels: Vec<u8>) -> Option<Self> {
        if pixels.len() != (w as usize) * (h as usize) * 4 { return None; }
        Some(Self { w, h, pixels })
    }
    pub fn dimensions(&self) -> (u32, u32) { (self.w, self.h) }
    pub fn get_pixel(&self, x: u32, y: u32) -> ([u8; 4], ()) {
        let i = ((y * self.w + x) * 4) as usize;
        let p = &self.pixels[i..i + 4];
        ([p[0], p[1], p[2], p[3]], ())
    }
}
pub struct DynamicImage;
impl DynamicImage {
    pub fn to_rgba8(&self) -> RgbaImage { RgbaImage { w: 0, h: 0, pixels: Vec::new() } }
}
pub enum ImageFormat { Dds }
pub fn load_from_memory_with_format(_b: &[u8], _f: ImageFormat) -> Result<DynamicImage, String> {
    Err("stub".into())
}
pub mod imageops {
    use super::RgbaImage;
    pub fn crop_imm(_img: &RgbaImage, _x: u32, _y: u32, _w: u32, _h: u32) -> Crop { Crop }
    pub struct Crop;
    impl Crop {
        pub fn to_image(self) -> RgbaImage { RgbaImage { w: 0, h: 0, pixels: Vec::new() } }
    }
}
#[allow(dead_code)]
fn _uses(_: DynamicImage) {}
EOF
        "$RUSTC" --crate-type rlib --crate-name image --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=0 "$OUT/water/image.rs" -o "$OUT/libimage_stub.rlib"
        "$RUSTC" --test --crate-name watertest --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=1 -L "$OUT" --extern flate2="$OUT/libflate2.rlib" \
            --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            --extern image="$OUT/libimage_stub.rlib" \
            "$OUT/water/lib.rs" -o "$OUT/watertest"
        shift || true
        (cd "$REPO" && exec "$OUT/watertest" "$@") ;;
    gfx)   toolchain; sysroot; deps; real_deps
        # The GFX containers on their own: `gfx_file`, `gfx_decal`, `gfx_light`
        # and the `pck` they read from, without the renderer or the `main.rs`
        # macros the full harness cannot resolve. Version-gate changes live here.
        mkdir -p "$OUT/gfx"
        cat > "$OUT/gfx/lib.rs" <<EOF
#[path = "$REPO/src/animation.rs"]
pub mod animation;
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
#[path = "$REPO/src/pck.rs"]
pub mod pck;
#[path = "$REPO/src/scene_lights.rs"]
pub mod scene_lights;
EOF
        "$RUSTC" --test --crate-name gfxver --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=1 -L "$OUT" --extern glam="$OUT/libglam.rlib" \
            --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            --extern flate2="$OUT/libflate2.rlib" \
            "$OUT/gfx/lib.rs" -o "$OUT/gfxver"
        shift || true
        (cd "$REPO" && exec "$OUT/gfxver" "$@") ;;
    music) toolchain; sysroot; deps; real_deps
        # `src/music.rs` alone: precinct parsing and the zone fallback points the
        # teleport list uses when a map ships no `world_targets.txt`.
        mkdir -p "$OUT/music"
        cat > "$OUT/music/lib.rs" <<EOF
#[path = "$OUT/pck_stub.rs"]
pub mod pck;
#[path = "$REPO/src/music.rs"]
pub mod music;
EOF
        "$RUSTC" --test --crate-name musictest --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=1 -L "$OUT" \
            --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            --extern flate2="$OUT/libflate2.rlib" \
            "$OUT/music/lib.rs" -o "$OUT/musictest"
        shift || true
        (cd "$REPO" && exec "$OUT/musictest" "$@") ;;
    material) toolchain; sysroot; deps; real_deps
        # The terrain material set: the missing-texture placeholder the client
        # substitutes must be a DDS the sampler and the DXT decoder both accept.
        mkdir -p "$OUT/mat"
        cat > "$OUT/mat/lib.rs" <<EOF
#[path = "$OUT/pck_stub.rs"]
pub mod pck;
#[path = "$REPO/src/dxt.rs"]
pub mod dxt;
#[path = "$REPO/src/terrain_sampling.rs"]
pub mod terrain_sampling;
#[path = "$REPO/src/material.rs"]
pub mod material;

pub fn u32at(b: &[u8], p: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4).ok_or("truncated")?.try_into().map_err(|_| "truncated")?,
    ))
}
pub fn f32at(b: &[u8], p: usize) -> Result<f32, String> {
    Ok(f32::from_bits(u32at(b, p)?))
}
EOF
        "$RUSTC" --test --crate-name mattest --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=1 -L "$OUT" --extern glam="$OUT/libglam.rlib" \
            --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            --extern flate2="$OUT/libflate2.rlib" \
            "$OUT/mat/lib.rs" -o "$OUT/mattest"
        shift || true
        (cd "$REPO" && exec "$OUT/mattest" "$@") ;;    fmt)   toolchain
        rustfmt --check --edition 2021 \
            "$REPO/src/gfx_player.rs" "$REPO/src/gfx_file.rs" "$REPO/src/gfx_decal.rs" \
            "$REPO/src/gfx_color_track.rs" "$REPO/src/gfx_catalog.rs" "$REPO/src/ecmodel.rs" \
            "$REPO/src/gfx_light.rs" "$REPO/src/gfx_owner.rs" "$REPO/src/scene_lights.rs" \
            "$REPO/src/view_settings.rs" "$REPO/src/client_weather.rs" \
            "$REPO/src/sky_transition.rs" "$REPO/src/scene.rs" "$REPO/src/model_dds.rs" "$REPO/src/skin.rs" "$REPO/src/skeleton.rs" "$REPO/src/skeleton_pose.rs" "$REPO/src/skin_pose.rs" "$REPO/src/model_asset.rs" "$REPO/src/model_material.rs" ;;
    *) echo "usage: $0 {setup|check|test|fmt|client|water|gfx|music|material}" >&2; exit 2 ;;esac
