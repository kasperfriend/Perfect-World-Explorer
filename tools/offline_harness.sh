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
#   tools/offline_harness.sh content  read the files INSIDE every staged pack
#                                     against the REAL goblin/flate2/sha2
#   tools/offline_harness.sh water    build+test `src/water.rs` against the REAL
#                                     flate2 (BSData records are zlib-framed)
#   tools/offline_harness.sh gfx      build+test the GFX containers (file, decal,
#                                     light) and the `pck` they read from
#   tools/offline_harness.sh player   build+test `src/gfx_player.rs` and the model
#                                     stack it links, so an edit to the player is
#                                     compiled outside CI too
#   tools/offline_harness.sh music    build+test `src/music.rs` (precinct zones and
#                                     the teleport fallback points)
#   tools/offline_harness.sh material build+test the terrain material set and the
#                                     missing-texture placeholder the client paints#
#   tools/offline_harness.sh scene    run the real `src/scene.rs` reader over every
#                                     ECWLD in a directory, so cross-version map
#                                     coverage is measured rather than assumed
#   tools/offline_harness.sh pck      read every entry of every `.pck` in a
#                                     directory with the real `src/pck.rs`, so a
#                                     reader change is checked against retail
#                                     packs and not only synthetic ones
#   tools/offline_harness.sh matrix   report every staged client side by side:
#                                     executable, data versions, package versions,
#                                     scenery and water, so cross-version coverage
#                                     is one table instead of several probes
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
# The `image` API surface the offline module set names, as a stub: this
# toolchain has no `image` crate, and these binaries only need the types to
# type-check. Built here rather than inside one target, so a stub that gains an
# item is not left stale for every other target that never rebuilds it.
image_stub() {
    [ -f "$OUT/libimage_stub.rlib" ] && return 0
    # `bins` can run before `water` has ever created this directory.
    mkdir -p "$OUT/water"
    cat > "$OUT/water/image.rs" <<'EOF'
//! The few `image` items `src/water.rs` names outside the decode path.
#[derive(Clone)]
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
impl RgbaImage {
    pub fn width(&self) -> u32 { self.w }
    pub fn height(&self) -> u32 { self.h }
    pub fn pixels(&self) -> std::slice::Iter<'_, u8> { self.pixels.iter() }
    pub fn as_raw(&self) -> &Vec<u8> { &self.pixels }
    pub fn into_raw(self) -> Vec<u8> { self.pixels }
    pub fn new(w: u32, h: u32) -> Self {
        Self { w, h, pixels: vec![0; (w as usize) * (h as usize) * 4] }
    }
    pub fn from_pixel(w: u32, h: u32, p: Rgba<u8>) -> Self {
        Self { w, h, pixels: p.0.iter().copied().cycle().take((w as usize) * (h as usize) * 4).collect() }
    }
}
#[derive(Clone, Copy)]
pub struct Rgba<T>(pub [T; 4]);
impl Rgba<u8> {
    pub fn channels(&self) -> [u8; 4] { self.0 }
}
pub struct DynamicImage;
impl DynamicImage {
    pub fn to_rgba8(&self) -> RgbaImage { RgbaImage { w: 0, h: 0, pixels: Vec::new() } }
    pub fn into_rgba8(self) -> RgbaImage { RgbaImage { w: 0, h: 0, pixels: Vec::new() } }
    pub fn width(&self) -> u32 { 0 }
    pub fn height(&self) -> u32 { 0 }
}
pub enum ImageFormat { Dds, Png, Tga }
pub fn load_from_memory_with_format(_b: &[u8], _f: ImageFormat) -> Result<DynamicImage, String> {
    Err("stub".into())
}
pub fn load_from_memory(_b: &[u8]) -> Result<DynamicImage, String> { Err("stub".into()) }
pub fn open(_p: impl AsRef<std::path::Path>) -> Result<DynamicImage, String> { Err("stub".into()) }
pub fn save_buffer(_p: impl AsRef<std::path::Path>, _b: &[u8], _w: u32, _h: u32, _c: ()) -> Result<(), String> {
    Err("stub".into())
}

pub mod imageops {
    use super::RgbaImage;
    pub fn resize(img: &RgbaImage, w: u32, h: u32, _f: FilterType) -> RgbaImage {
        RgbaImage { w, h, pixels: img.pixels.clone() }
    }
    pub fn grayscale(_img: &RgbaImage) -> RgbaImage { RgbaImage { w: 0, h: 0, pixels: Vec::new() } }
    pub fn flip_vertical(_img: &RgbaImage) -> RgbaImage { RgbaImage { w: 0, h: 0, pixels: Vec::new() } }
    pub enum FilterType { Nearest, Triangle }
    pub fn resize_exact(img: &RgbaImage, w: u32, h: u32, _f: FilterType) -> RgbaImage {
        RgbaImage { w, h, pixels: img.pixels.clone() }
    }
    pub fn replace(_dst: &mut RgbaImage, _src: &RgbaImage, _x: i64, _y: i64) {}
    pub fn crop_imm(_img: &RgbaImage, _x: u32, _y: u32, _w: u32, _h: u32) -> Crop { Crop }
    pub struct Crop;
    impl Crop {
        pub fn to_image(self) -> RgbaImage { RgbaImage { w: 0, h: 0, pixels: Vec::new() } }
    }
}
#[allow(dead_code)]
fn _uses(_: DynamicImage) {}
EOF
    "$RUSTC" --crate-type rlib --crate-name image --edition 2021 \
        --sysroot "$SYSROOT" -C opt-level=0 "$OUT/water/image.rs" \
        -o "$OUT/libimage_stub.rlib"
}

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

/// The real `pck::inflate`, against the real `flate2`. `angelica_pck` decodes
/// its payloads through this, so a stand-in that refuses made its own test fail
/// for a reason that had nothing to do with it.
pub(crate) fn inflate(raw: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    use flate2::{Decompress, FlushDecompress};
    let mut out = vec![0u8; expected];
    let mut decomp = Decompress::new(true);
    decomp
        .decompress(raw, &mut out, FlushDecompress::Finish)
        .map_err(|e| e.to_string())?;
    out.truncate(decomp.total_out() as usize);
    Ok(out)
}

pub fn corpus_absent(_what: &str) {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// Offset from the start of the package, not of one file; a package may be
    /// longer than 4 GiB.
    pub offset: u64,
    pub size: u32,
    pub compressed: u32,
}

pub fn directory(_f: &mut File) -> Result<Vec<Entry>, String> {
    Err("offline harness: no zlib backend".to_string())
}

pub fn read_entry(f: &mut File, e: &Entry) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};
    f.seek(SeekFrom::Start(e.offset)).map_err(|x| x.to_string())?;
    let mut raw = vec![0u8; e.compressed as usize];
    f.read_exact(&mut raw).map_err(|x| x.to_string())?;
    if e.compressed < e.size {
        inflate(&raw, e.size as usize)
    } else {
        Ok(raw)
    }
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
// \`gfx_owner\` and \`client_weather\` reach for \`crate::diag_keyed!\` and
// \`crate::diagnostics\`, and the macro is exported from the crate root, so the
// real module has to be in the set -- without it \`check\` failed with 44
// "could not find \`diag_keyed\` in the crate root" errors and could not be used.
#[path = "$REPO/src/model_playback.rs"]
pub mod model_playback;
// The world-model module builds SceneModel in its own tests, so a field added
// to that struct breaks it -- which is exactly what the CI binary check caught
// while this module set did not contain this module at all.
#[path = "$REPO/src/world_models.rs"]
pub mod world_models;
#[path = "$REPO/src/world_catalog.rs"]
pub mod world_catalog;
#[path = "$OUT/pck_stub.rs"]
pub mod pck;
// The zone-name table: its own tests, and its data must keep matching the
// shapes it claims.
#[path = "$REPO/src/world_place_names.rs"]
pub mod world_place_names;
// \`model_playback\`'s tests build their asset from the same fixture the
// shipped binary mounts under \`#[cfg(test)]\`; without it this suite stopped at
// "could not find \`model_draw_fixture\` in the crate root" before running a
// single test.
#[cfg(test)]
#[path = "$REPO/tools/fixtures/model_draw.rs"]
pub mod model_draw_fixture;
// \`client_detect\` dispatches to the Angelica reader for the config and shader
// packs, so the harness needs it or the whole set fails to type-check.
#[path = "$REPO/src/angelica_pck.rs"]
pub mod angelica_pck;
// The precinct reader. Its tests are the only proof that a version-7
// \`precinct.clt\`'s playlists and ambience are read at all, and they were not
// running: this module set did not contain the module, so \`test\` reported
// "174 passed" whether or not the parser worked.
#[path = "$REPO/src/music.rs"]
pub mod music;
// The T2BK/TRN2 header decoders. `terrain_block` names the versions it reads
// and the TRN2 reader is where "an instance map has no world transform" is
// proved, so both belong in the suite that runs without a GPU.
#[path = "$REPO/src/terrain_block.rs"]
pub mod terrain_block;
EOF
}

EXT="--extern glam=$OUT/libglam.rlib --extern encoding_rs=$OUT/libencoding_rs.rlib"

case "${1:-test}" in
    setup) toolchain; sysroot; deps; echo "harness ready in $OUT" ;;
    check) toolchain; sysroot; deps; real_deps
        # Type-check the modules that only need the dependencies this offline
        # toolchain has. `gfx_player`, `gfx_owner`, `view_settings` and
        # `diagnostics` are left out because they reach for `egui` and the fps
        # settings types the gui crate defines; pulling them in made this mode
        # report 44 resolution errors that were never about the code, so it
        # could not be used to check anything. Their own suites cover them.
        "$RUSTC" --crate-type lib --crate-name harness --edition 2021 --sysroot "$SYSROOT" \
            $EXT -L "$OUT" --extern flate2="$OUT/libflate2.rlib" \
            -C opt-level=0 "$OUT/lib.rs" -o "$OUT/libharness.rlib"
        echo "type-check clean" ;;
    bins)  toolchain; sysroot; deps; real_deps; image_stub
        # Type-check the `src/bin` targets whose whole dependency set exists
        # offline, in their test configuration, which is how CI builds them.
        #
        # `check` only covers the library module set, so a bug in a binary -- or
        # in the `#[test]` block of a `#[path]`-included module, which the binary
        # compiles -- reached CI unseen. `unwrap_err()` on a type without `Debug`
        # did exactly that. The window and GPU binaries need
        # `wgpu`/`egui`/`winit` and stay CI's job.
        mkdir -p "$OUT/bins"
        total=0
        failed=0
        for path in "$REPO"/src/bin/*.rs; do
            name="$(basename "$path" .rs)"
            # Skip a binary that names a crate this toolchain does not have.
            if grep -qE '(^|[^A-Za-z_])(wgpu|bytemuck|egui|egui_wgpu|egui_winit|winit)::' "$path"; then
                continue
            fi
            # A binary that reaches a `#[path]` module needing `Digest::new`:
            # the sorted `sha2` here is a stub without it.
            if grep -q "sha2::" "$path" || grep -rqs "Sha256::new" "$REPO/src/terrain_store.rs"; then
                case "$name" in terrain-memory-check|navigation-survey) continue ;; esac
            fi
            total=$((total + 1))
            if ! "$RUSTC" --crate-name "b_$(echo "$name" | tr '-' '_')" --edition 2021 \
                --test --sysroot "$SYSROOT" -L "$OUT" \
                $EXT --extern flate2="$OUT/libflate2.rlib" \
                --extern encoding_rs="$OUT/libencoding_rs.rlib" \
                --extern image="$OUT/libimage_stub.rlib" \
                --extern goblin="$OUT/libgoblin.rlib" \
                --extern sha2="$OUT/libsha2.rlib" \
                -o "$OUT/bins/$name" "$path" 2>"$OUT/bins/$name.log"; then
                failed=$((failed + 1))
                echo "FAIL $name"
                grep -E "^error" -A4 "$OUT/bins/$name.log" | head -10
            fi
        done
        echo "bins: $((total - failed))/$total type-check clean"
        [ "$failed" -eq 0 ] ;;
    test)  toolchain; sysroot; deps; real_deps
        # `angelica_pck` is in the closure and it really decompresses, so the
        # real `flate2` is required here; without it the whole suite failed to
        # build on a missing crate rather than on its own code.
        "$RUSTC" --test --crate-name harness --edition 2021 --sysroot "$SYSROOT" \
            $EXT -L "$OUT" --extern flate2="$OUT/libflate2.rlib" \
            -C opt-level=1 "$OUT/lib.rs" -o "$OUT/gfxtest"
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
        # `water` needs flate2 (BSData records are zlib-framed). The parse path
        # now also names `image` for the simple-water frames, so the shim
        # `bins` builds has to exist here too.
        real_deps; image_stub
        mkdir -p "$OUT/water"
        # The reader reports its per-record failures through `crate::diag!`, so
        # the shim has to carry it: a stub that dropped the lines would make the
        # harness disagree with the viewer about which records load. It goes in
        # its own quoted heredoc, because the macro's `$` is not shell input and
        # the outer heredoc is unquoted.
        cat > "$OUT/water/diag_shim.rs" <<'EOF'
pub mod diagnostics {
    #[allow(dead_code)]
    #[derive(Clone, Copy)]
    pub enum Channel {
        SceneWater,
    }
    pub fn enabled(_channel: Channel) -> bool {
        false
    }
    pub fn emit(_channel: Channel, _line: String) {}
}
#[macro_export]
macro_rules! diag {
    ($channel:ident, $($arg:tt)*) => {{ let _ = format!($($arg)*); }};
}
EOF
        cat > "$OUT/water/lib.rs" <<EOF
#[path = "$OUT/water/diag_shim.rs"]
#[macro_use]
mod diag_shim;
pub use diag_shim::diagnostics;
pub mod scene {
    pub struct WaterRef { pub export_id: u32, pub offset: u32 }
    pub struct Block { pub water: Vec<WaterRef> }
    pub struct Scene { pub blocks: Vec<Block> }
    /// The real identity check, so the harness refuses what the viewer refuses.
    /// The water reader never looks below the revision word, so the revision is
    /// accepted whatever it is -- the same contract as the viewer's.
    pub const ECBSD_IDENTITY: (u32, u32) = (0x42584f4d, 0x42534446);
    pub fn ecbsd_header(b: &[u8]) -> Result<u32, String> {
        let at = |p: usize| -> Result<u32, String> {
            b.get(p..p + 4)
                .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
                .ok_or_else(|| "truncated EC-BSD header".to_string())
        };
        let identity = (at(0)?, at(4)?);
        if identity != ECBSD_IDENTITY {
            return Err(format!(
                "this is not an EC-BSD file: it starts with {:#010x} {:#010x}",
                identity.0, identity.1
            ));
        }
        at(8)
    }
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
        "$RUSTC" --test --crate-name watertest --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=1 -L "$OUT" --extern flate2="$OUT/libflate2.rlib" \
            --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            --extern image="$OUT/libimage_stub.rlib" \
            "$OUT/water/lib.rs" -o "$OUT/watertest"
        shift || true
        (cd "$REPO" && exec "$OUT/watertest" "$@") ;;
    water-record-survey) toolchain; sysroot; deps; real_deps; image_stub
        # The water reference classifier, built from the real readers. Its
        # `--classify` mode buckets every water reference under a client's
        # `element/maps` and exits non-zero if the buckets do not add up, which
        # is what keeps "148 references, 3 areas" an answered question.
        mkdir -p "$OUT/survey"
        "$RUSTC" --edition 2021 --crate-name wrsurvey --sysroot "$SYSROOT" \
            -L "$OUT" --extern flate2="$OUT/libflate2.rlib" \
            --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            --extern image="$OUT/libimage_stub.rlib" \
            -o "$OUT/survey/water-record-survey" \
            "$REPO/src/bin/water-record-survey.rs"
        shift || true
        [ $# -gt 0 ] || set -- "$REPO/google_drive"
        (cd "$REPO" && exec "$OUT/survey/water-record-survey" "$@") ;;
    music) toolchain; sysroot; deps; real_deps
        # Read every `precinct.clt` under the given roots with the real reader.
        # The map's music, its stereo ambience and its teleport list all come
        # from this one UTF-16 file, and one unreadable zone used to cost the
        # whole file. Prints per file: zones, playlist tracks, zones with an
        # ambience path, and the version word.
        cat > "$OUT/music_probe.rs" <<EOF
#[path = "$REPO/src/pck.rs"]
mod pck;
#[path = "$REPO/src/music.rs"]
mod music;
fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() { walk(&p, out); }
        else if p.file_name().and_then(|n| n.to_str()).map(|n| n.eq_ignore_ascii_case("precinct.clt")).unwrap_or(false) { out.push(p); }
    }
}
fn main() {
    let mut files = Vec::new();
    for root in std::env::args().skip(1) { walk(std::path::Path::new(&root), &mut files); }
    if files.is_empty() { eprintln!("no precinct.clt under the given roots"); std::process::exit(2); }
    println!("{:<58} {:>7} {:>7} {:>7} {:>7}", "file", "version", "zones", "tracks", "amb");
    let mut bad = 0;
    for f in &files {
        let version = std::fs::read(f).ok()
            .map(|b| b.chunks_exact(2).map(|x| u16::from_le_bytes([x[0], x[1]])).collect::<Vec<_>>())
            .and_then(|w| String::from_utf16(&w).ok())
            .and_then(|t| t.lines().find(|l| l.trim().starts_with("version"))
                .and_then(|l| l.split_whitespace().nth(1).map(str::to_owned)))
            .unwrap_or_else(|| "?".into());
        match music::Zones::open(f) {
            Ok(z) => {
                let tracks: usize = z.zones.iter().map(|x| x.tracks.len()).sum();
                let amb = z.zones.iter().filter(|x| !x.ambience_day.is_empty() || !x.ambience_night.is_empty()).count();
                println!("{:<58} {:>7} {:>7} {:>7} {:>7}", f.display().to_string(), version, z.zones.len(), tracks, amb);
            }
            Err(e) => { println!("{:<58} {:>7} FAILED: {e}", f.display().to_string(), version); bad += 1; }
        }
    }
    std::process::exit(if bad == 0 { 0 } else { 1 });
}
EOF
        "$RUSTC" --edition 2021 --crate-name musicprobe --sysroot "$SYSROOT" \
            -L "$OUT" --extern flate2="$OUT/libflate2.rlib" \
            --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            -o "$OUT/music-probe" "$OUT/music_probe.rs"
        shift || true
        [ $# -gt 0 ] || set -- "$REPO/google_drive"
        (cd "$REPO" && exec "$OUT/music-probe" "$@") ;;
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
    player) toolchain; sysroot; deps; real_deps
        # `src/gfx_player.rs` and the modules it links against.
        #
        # This exists because `check` cannot compile the player: it reaches for
        # `egui` through `diagnostics` and for the model stack, so it was left
        # out of the type-check set and *nothing* outside CI compiled it. A
        # three-line edit to the colour-ramp arm — adding a field to
        # `ControlData::ColorRamp` and reading it in the `Process` match — then
        # shipped two type errors that `check`, `gfx` and `test` all passed over,
        # because none of them build this file. The stubs below are the smallest
        # set that lets the real parser, player and their suites compile:
        # `diagnostics` as a no-op, the diag macros as no-ops.
        mkdir -p "$OUT/player"
        # The two macros carry `$`, which the unquoted heredoc below would try
        # to expand, so they get their own quoted heredoc.
        cat > "$OUT/player/macros.rs" <<'EOF'
#[macro_export]
macro_rules! diag { ($($t:tt)*) => {}; }
#[macro_export]
macro_rules! diag_keyed { ($($t:tt)*) => {}; }
EOF
        cat > "$OUT/player/lib.rs" <<EOF
#[path = "$OUT/player/macros.rs"]
#[macro_use]
mod macros;
pub mod diagnostics {
    #[derive(Clone, Copy)]
    pub enum Channel {
        EffectCards,
        EffectElements,
        EffectFrames,
        EffectUv,
    }
    pub fn enabled(_channel: Channel) -> bool { false }
    pub fn any() -> bool { false }
}
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
#[path = "$REPO/src/gfx_player.rs"]
pub mod gfx_player;
#[path = "$REPO/src/model_asset.rs"]
pub mod model_asset;
#[path = "$REPO/src/model_dds.rs"]
pub mod model_dds;
#[path = "$REPO/tools/fixtures/model_draw.rs"]
pub mod model_draw_fixture;
#[path = "$REPO/src/model_material.rs"]
pub mod model_material;
#[path = "$REPO/src/model_playback.rs"]
pub mod model_playback;
#[path = "$REPO/src/pck.rs"]
pub mod pck;
#[path = "$REPO/src/skeleton.rs"]
pub mod skeleton;
#[path = "$REPO/src/skeleton_pose.rs"]
pub mod skeleton_pose;
#[path = "$REPO/src/skin.rs"]
pub mod skin;
#[path = "$REPO/src/skin_pose.rs"]
pub mod skin_pose;
EOF
        "$RUSTC" --test --crate-name playertest --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=1 -L "$OUT" --extern glam="$OUT/libglam.rlib" \
            --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            --extern flate2="$OUT/libflate2.rlib" \
            "$OUT/player/lib.rs" -o "$OUT/playertest"
        shift || true
        (cd "$REPO" && exec "$OUT/playertest" "$@") ;;
    music) toolchain; sysroot; deps; real_deps
        # `src/music.rs` alone: precinct parsing and the zone fallback points the
        # teleport list uses when a map ships no `world_targets.txt`.
        mkdir -p "$OUT/music"
        cat > "$OUT/music/lib.rs" <<EOF
#[path = "$OUT/pck_stub.rs"]
pub mod pck;
#[path = "$REPO/src/music.rs"]
pub mod music;
// The T2BK/TRN2 header decoders. `terrain_block` names the versions it reads
// and the TRN2 reader is where "an instance map has no world transform" is
// proved, so both belong in the suite that runs without a GPU.
#[path = "$REPO/src/terrain_block.rs"]
pub mod terrain_block;
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
            "$REPO/src/sky_transition.rs" "$REPO/src/scene.rs" "$REPO/src/model_dds.rs" "$REPO/src/skin.rs" "$REPO/src/skeleton.rs" "$REPO/src/skeleton_pose.rs" "$REPO/src/skin_pose.rs" "$REPO/src/model_asset.rs" "$REPO/src/model_material.rs" \
            "$REPO/src/pck.rs" "$REPO/src/client_detect.rs" ;;
    scene) toolchain; sysroot; deps
        # Sweep real `.ecwld` files with the real reader. Map files are the only
        # scenery input, and the body layouts they carry differ by client
        # version, so a directory of them is the honest test corpus.
        mkdir -p "$OUT/scene"
        cat > "$OUT/scene/sweep.rs" <<EOF
#[path = "$REPO/src/scene.rs"]
pub mod scene;
use std::path::Path;
fn main() {
    let dir = std::env::args().nth(1).expect("ecwld directory");
    let mut files: Vec<_> = std::fs::read_dir(Path::new(&dir))
        .expect("read directory").flatten().map(|e| e.path()).collect();
    files.sort();
    let (mut ok, mut failed) = (0u32, 0u32);
    for p in files {
        if p.extension().and_then(|e| e.to_str()) != Some("ecwld") { continue; }
        let data = std::fs::read(&p).expect("read ecwld");
        let version = data.get(4..8)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .unwrap_or(0);
        match scene::Scene::parse(&data) {
            Ok(s) => {
                ok += 1;
                let mut n = [0usize; 10];
                for b in &s.blocks {
                    n[0]+=b.trees.len(); n[1]+=b.water.len(); n[2]+=b.grass.len();
                    n[3]+=b.ornaments.len(); n[4]+=b.boxes.len(); n[5]+=b.effects.len();
                    n[6]+=b.models.len(); n[7]+=b.critters.len(); n[8]+=b.beziers.len();
                    n[9]+=b.sounds.len();
                }
                println!("ok   {:24} v{:<3} blocks={:<6} {}x{} bs={:.0} types={}/{} trees={} water={} grass={} orn={} boxes={} fx={} models={} critters={} bez={} snd={}",
                    p.file_name().unwrap().to_string_lossy(), version, s.blocks.len(),
                    s.rows, s.cols, s.block_size, s.tree_types.len(), s.grass_types.len(),
                    n[0],n[1],n[2],n[3],n[4],n[5],n[6],n[7],n[8],n[9]);
            }
            Err(error) => {
                failed += 1;
                println!("FAIL {:24} v{:<3} {error}",
                    p.file_name().unwrap().to_string_lossy(), version);
            }
        }
    }
    println!("ecwld: {ok} ok, {failed} failed");
    if failed > 0 { std::process::exit(1); }
}
EOF
        "$RUSTC" --crate-name scenesweep --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=2 -L "$OUT" --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            "$OUT/scene/sweep.rs" -o "$OUT/scenesweep"
        shift || true
        exec "$OUT/scenesweep" "${1:-$REPO/google_drive/scenecorpus}" ;;
    pck) toolchain; sysroot; deps
        # Decode every entry of every pack in the directory. A directory listing
        # alone would pass while a record walk silently desynchronised, so the
        # check is that each entry decompresses to the length its record claims.
        mkdir -p "$OUT/pck"
        cat > "$OUT/pck/sweep.rs" <<EOF
#[path = "$REPO/src/pck.rs"]
pub mod pck;
#[path = "$REPO/src/angelica_pck.rs"]
pub mod angelica_pck;
use std::path::Path;
fn main() {
    let dir = std::env::args().nth(1).expect("pck directory");
    let mut files: Vec<_> = std::fs::read_dir(Path::new(&dir))
        .expect("read directory").flatten().map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("pck"))
        .collect();
    files.sort();
    let (mut good, mut bad) = (0u32, 0u32);
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let mut file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => { println!("FAIL {name}: {e}"); bad += 1; continue; }
        };
        // Two containers share the v2 prefix and the flag byte, and the client
        // tells them apart by their guard pair: the older \`0xa508bdc4\` /
        // \`0x853567c8\` Angelica packs and the newer \`0xfdfdfeee\` / \`0xf00dbeef\`
        // ones. Try both, the way \`client_detect::scan_archive\` does, so a pack
        // the Angelica reader handles is not reported as a failure here.
        let (source, entries) = match pck::directory(&mut file) {
            Ok(entries) => ("pck", Ok(entries)),
            Err(error) => ("angelica", angelica_pck::directory(&mut file).map_err(|inner| {
                format!("{error}; {inner}")
            })),
        };
        match entries {
            Ok(entries) => {
                let mut wrong = 0usize;
                let mut first = String::new();
                for e in &entries {
                    // Both containers carry absolute entry offsets in the same
                    // Entry shape, so one reader serves them.
                    let read = pck::read_entry(&mut file, e);
                    match read {
                        Ok(b) if b.len() == e.size as usize => {}
                        Ok(b) => {
                            wrong += 1;
                            if first.is_empty() {
                                first = format!("{} decoded to {} not {}", e.name, b.len(), e.size);
                            }
                        }
                        Err(err) => {
                            wrong += 1;
                            if first.is_empty() { first = format!("{}: {err}", e.name); }
                        }
                    }
                }
                if wrong == 0 {
                    good += 1;
                    println!("ok   {name:<22} {} entries ({source})", entries.len());
                } else {
                    bad += 1;
                    println!("FAIL {name:<22} {} entries, {wrong} unreadable ({first})", entries.len());
                }
            }
            Err(error) => { bad += 1; println!("FAIL {name:<22} {error}"); }
        }
    }
    println!("pck: {good} ok, {bad} failed");
    if bad > 0 { std::process::exit(1); }
}
EOF
        "$RUSTC" --crate-name pcksweep --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=2 -L "$OUT" --extern flate2="$OUT/libflate2.rlib" \
            --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            "$OUT/pck/sweep.rs" -o "$OUT/pcksweep"
        shift || true
        exec "$OUT/pcksweep" "${1:-$REPO/google_drive/packs}" ;;
    content) toolchain; sysroot; deps
        # Read the files *inside* the packages, not just their directories. A
        # pack whose directory parses but whose payloads do not is not readable,
        # and `pck` above cannot tell the difference. Each extension is checked
        # with the reader that owns it, or with its magic where no reader exists
        # (`.mox`, `.spt`) so an unread format is counted rather than claimed.
        mkdir -p "$OUT/content"
        cat > "$OUT/content/sweep.rs" <<EOF
#[path = "$REPO/src/angelica_pck.rs"]
pub mod angelica_pck;
#[path = "$REPO/src/gfx_file.rs"]
pub mod gfx_file;
#[path = "$REPO/src/pck.rs"]
pub mod pck;

use std::collections::BTreeMap;
use std::fs::File;

struct Tally { ok: u64, failed: u64, first: Vec<String> }
impl Tally {
    fn new() -> Self { Tally { ok: 0, failed: 0, first: Vec::new() } }
    fn record(&mut self, name: &str, r: Result<(), String>) {
        match r {
            Ok(()) => self.ok += 1,
            Err(e) => {
                self.failed += 1;
                if self.first.len() < 2 { self.first.push(format!("{name}: {e}")); }
            }
        }
    }
}

fn main() {
    let dir = std::env::args().nth(1).expect("pack directory");
    let mut paths: Vec<_> = std::fs::read_dir(&dir).expect("read dir").flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("pck"))
        .collect();
    paths.sort();
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    for path in &paths {
        let mut f = File::open(path).expect("open pack");
        let entries = match pck::directory(&mut f) {
            Ok(e) => e,
            Err(_) => match angelica_pck::directory(&mut f) { Ok(e) => e, Err(_) => continue },
        };
        for e in &entries {
            let ext = e.name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            if !matches!(ext.as_str(),
                "gfx" | "gfx_err" | "sdr" | "ski" | "bon" | "mox" | "spt"
                | "dds" | "tga" | "bmp" | "jpg" | "ttf") {
                continue;
            }
            let payload = match pck::read_entry(&mut f, e) {
                Ok(p) => p,
                Err(err) => {
                    tallies.entry(ext).or_insert_with(Tally::new)
                        .record(&e.name, Err(format!("decode: {err}")));
                    continue;
                }
            };
            let t = tallies.entry(ext.clone()).or_insert_with(Tally::new);
            match ext.as_str() {
                "gfx" | "gfx_err" => {
                    // GFX containers are GBK, never UTF-8.
                    let (text, damaged) = gfx_file::decode(&payload);
                    let r = gfx_file::parse(&text).map(|_| ()).map_err(|err| {
                        if damaged { format!("{err} (and the text had GBK damage)") } else { err }
                    });
                    t.record(&e.name, r);
                }
                "sdr" => {
                    let (text, _) = gfx_file::decode(&payload);
                    let ok = text.trim_start().starts_with("//") || text.contains("void main")
                        || text.contains("technique") || text.contains("sampler");
                    t.record(&e.name, if ok { Ok(()) } else {
                        Err(format!("not shader text: {:?}", text.chars().take(40).collect::<String>()))
                    });
                }
                "ski" | "bon" => {
                    t.record(&e.name, if payload.len() >= 8 { Ok(()) } else { Err("too short".into()) });
                }
                "mox" => {
                    // Recognised, but this project has no reader for it.
                    t.record(&e.name, if payload.starts_with(b"MOXBFRAME:") { Ok(()) } else {
                        Err(format!("not MOXBFRAME: {:?}", String::from_utf8_lossy(&payload[..payload.len().min(20)])))
                    });
                }
                "spt" => {
                    // SpeedTree source; the magic sits at offset 8 behind a
                    // small header every staged file shares.
                    let at = payload.windows(10).position(|w| w == b"__IdvSpt_0");
                    t.record(&e.name, if at.is_some() { Ok(()) } else {
                        Err(format!("no __IdvSpt_ magic in {} bytes", payload.len()))
                    });
                }
                // Images are the largest group here (226 MB of DDS alone) and
                // the offline toolchain has only a stub `image`, so the header
                // is validated rather than the pixels. A truncated file still
                // fails, which is the failure that actually happens.
                "dds" => {
                    let r = (|| -> Result<(), String> {
                        if payload.len() < 128 { return Err(format!("{} bytes, header is 128", payload.len())); }
                        if &payload[..4] != b"DDS " { return Err("no DDS magic".into()); }
                        let hdr = u32::from_le_bytes(payload[4..8].try_into().unwrap());
                        if hdr != 124 { return Err(format!("DDS_HEADER size {hdr}, expected 124")); }
                        let h = u32::from_le_bytes(payload[12..16].try_into().unwrap());
                        let w = u32::from_le_bytes(payload[16..20].try_into().unwrap());
                        if w == 0 || h == 0 || w > 8192 || h > 8192 {
                            return Err(format!("implausible {w}x{h}"));
                        }
                        Ok(())
                    })();
                    t.record(&e.name, r);
                }
                "tga" => {
                    // No magic: the type field at offset 2 is the only signal.
                    let r = (|| -> Result<(), String> {
                        if payload.len() < 19 { return Err(format!("{} bytes", payload.len())); }
                        let kind = payload[2];
                        if !matches!(kind, 0 | 1 | 2 | 3 | 9 | 10 | 11) {
                            return Err(format!("TGA image type {kind}"));
                        }
                        Ok(())
                    })();
                    t.record(&e.name, r);
                }
                "bmp" => {
                    t.record(&e.name, if payload.len() >= 26 && &payload[..2] == b"BM" { Ok(()) }
                        else { Err(format!("{} bytes, not BM", payload.len())) });
                }
                "jpg" => {
                    t.record(&e.name,
                        if payload.len() >= 4 && payload[..2] == [0xff, 0xd8] { Ok(()) }
                        else { Err(format!("{} bytes, not FFD8", payload.len())) });
                }
                "ttf" => {
                    // TrueType outlines start 00 01 00 00, CFF-based OTTO, and
                    // a collection ttcf. Reading the glyphs needs a font
                    // library this project does not carry.
                    let r = (|| -> Result<(), String> {
                        if payload.len() < 12 { return Err(format!("{} bytes", payload.len())); }
                        match &payload[..4] {
                            b"\x00\x01\x00\x00" | b"OTTO" | b"true" | b"ttcf" => Ok(()),
                            other => Err(format!("not a font sfnt: {other:02x?}")),
                        }
                    })();
                    t.record(&e.name, r);
                }
                _ => {}
            }
        }
    }
    let (mut ok, mut failed) = (0u64, 0u64);
    println!("{:<10} {:>7} {:>7}", "ext", "ok", "failed");
    for (ext, t) in &tallies {
        ok += t.ok; failed += t.failed;
        println!("{:<10} {:>7} {:>7}", ext, t.ok, t.failed);
        for line in &t.first { println!("        - {line}"); }
    }
    println!("\ncontent: {ok} read, {failed} failed");
    // Both retail gfx.pck builds ship one authored stub -- four bytes, "MOXT"
    // -- so a reader cannot parse it and that is the archive's content, not a
    // failure. It is reported above and excluded from the exit status; any
    // other failure fails the run.
    // Likewise `interfaces/iconlist_guild.dds` in the 1.5.2 interfaces pack:
    // 1158 entries, 0 overlapping spans, and this one member is x86 code with
    // runs of '$' and three embedded, bogus "DDS " strings. 3020 of the 3021
    // staged .dds members carry a valid header, so this is the archive's
    // content, not a misread offset. Named here so it is not mistaken for one.
    let mut unexpected = 0u64;
    for (ext, t) in &tallies {
        for line in &t.first {
            if ext == "gfx" && line.contains("holds no fields, only \"MOXT\"") { continue; }
            if ext == "dds" && line.contains("iconlist_guild.dds") { continue; }
            unexpected += 1;
        }
    }
    if unexpected > 0 { std::process::exit(1); }
}
EOF
        "$RUSTC" --crate-name contentsweep --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=2 -L "$OUT" --extern flate2="$OUT/libflate2.rlib" \
            --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            --extern glam="$OUT/libglam.rlib" \
            "$OUT/content/sweep.rs" -o "$OUT/contentsweep"
        shift || true
        exec "$OUT/contentsweep" "${1:-$REPO/google_drive/packs}" ;;
    matrix) toolchain; sysroot; deps; real_deps
        # One row per staged client. Everything here is produced by the real
        # readers; nothing is asserted about a version that is not present.
        mkdir -p "$OUT/matrix"
        cat > "$OUT/matrix/run.rs" <<EOF
#[path = "$REPO/src/client_detect.rs"]
pub mod client_detect;
#[path = "$REPO/src/pck.rs"]
pub mod pck;
#[path = "$REPO/src/angelica_pck.rs"]
pub mod angelica_pck;
#[path = "$REPO/src/scene.rs"]
pub mod scene;
#[path = "$OUT/water/image.rs"]
pub mod image;
#[path = "$REPO/src/water.rs"]
pub mod water;
use std::path::Path;
fn main() {
    let root = std::env::args().nth(1).expect("clients dir");
    let mut dirs: Vec<_> = std::fs::read_dir(&root).expect("read clients dir")
        .flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for dir in dirs {
        println!("\n################ {} ################", dir.display());
        let pick = if dir.join("element").is_dir() { dir.join("element") } else { dir.clone() };
        let scan = client_detect::scan(&pick);
        println!("element     : {}", scan.element.display());
        println!("readable_map: {:?}", scan.readable_map);
        println!("loadable    : {}", scan.loadable());
        for e in &scan.errors { println!("ERROR       : {e}"); }
        for n in &scan.notes { println!("NOTE        : {n}"); }
        if let Some(exe) = &scan.executable {
            println!("exe         : {} | pe32={} known={:?} filever={:?}",
                exe.label(), exe.pe32, exe.known, exe.file_version);
        }
        println!("game_version: {}", scan.game_version());
        println!("data_version: {}", scan.data_version_summary());
        for a in &scan.archives {
            println!("archive     : {:22} kind={:?} v={:?} entries={:?} problem={:?}",
                a.name, a.kind, a.version, a.entries, a.problem);
        }
        for d in &scan.data_files {
            println!("datafile    : {:22} version={:?} problem={:?}", d.name, d.version, d.problem);
        }
        for line in scenery(&pick) { println!("{line}"); }
        for line in water_areas(&pick) { println!("{line}"); }
    }
}
fn scenery(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root.join("maps")) else { return out };
    let mut files: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    files.sort();
    for map in files.iter().filter(|p| p.is_dir()) {
        let Ok(rd) = std::fs::read_dir(map) else { continue };
        for f in rd.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("ecwld") { continue; }
            let data = match std::fs::read(&p) { Ok(d) => d, Err(_) => continue };
            match scene::Scene::parse(&data) {
                Ok(s) => {
                    let mut n = [0usize; 10];
                    for b in &s.blocks {
                        n[0]+=b.trees.len(); n[1]+=b.water.len(); n[2]+=b.grass.len();
                        n[3]+=b.ornaments.len(); n[4]+=b.boxes.len(); n[5]+=b.effects.len();
                        n[6]+=b.models.len(); n[7]+=b.critters.len(); n[8]+=b.beziers.len();
                        n[9]+=b.sounds.len();
                    }
                    out.push(format!("scene       : {:20} ok  blocks={} {}x{} types={}/{} trees={} water={} grass={} orn={} boxes={} fx={} models={} critters={} bez={} snd={}",
                        p.file_name().unwrap().to_string_lossy(), s.blocks.len(), s.rows, s.cols,
                        s.tree_types.len(), s.grass_types.len(),
                        n[0],n[1],n[2],n[3],n[4],n[5],n[6],n[7],n[8],n[9]));
                }
                Err(e) => out.push(format!("scene       : {:20} FAILED {e}",
                    p.file_name().unwrap().to_string_lossy())),
            }
        }
    }
    out
}
fn water_areas(root: &Path) -> Vec<String> {
    // Water lives in \`maps/<id>/<id>.ecbsd\` (a manifest of \`bsdata/*.dat\`
    // paths and per-ref record indices) and the areas themselves are inside
    // those \`.dat\` files. \`water::load\` is the reader that already knows that
    // shape, so drive it rather than re-deriving the framing here: an earlier
    // version of this report treated the last dword of the \`.ecbsd\` as a record
    // count and printed \`records=0\` for every map.
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root.join("maps")) else { return out };
    let mut dirs: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for map in dirs {
        let name = map.file_name().unwrap().to_string_lossy().to_string();
        let wld = map.join(format!("{name}.ecwld"));
        let Ok(data) = std::fs::read(&wld) else { continue };
        let Ok(scene) = scene::Scene::parse(&data) else { continue };
        let refs: usize = scene.blocks.iter().map(|b| b.water.len()).sum();
        let unique = {
            let mut set = std::collections::BTreeSet::new();
            for b in &scene.blocks { for w in &b.water { set.insert((w.export_id, w.offset)); } }
            set.len()
        };
        match water::load(&scene, &map) {
            Ok(areas) => out.push(format!(
                "water       : {:20} refs={} unique={} areas={}",
                format!("{name}.ecbsd"), refs, unique, areas.len())),
            Err(error) => out.push(format!(
                "water       : {:20} refs={} unique={} FAILED {error}",
                format!("{name}.ecbsd"), refs, unique)),
        }
    }
    out
}
EOF
        "$RUSTC" --crate-name matrix --edition 2021 --sysroot "$SYSROOT" \
            -C opt-level=2 -L "$OUT" \
            --extern flate2="$OUT/libflate2.rlib" \
            --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            --extern sha2="$OUT/libsha2.rlib" \
            --extern goblin="$OUT/libgoblin.rlib" \
            --extern image="$OUT/libimage_stub.rlib" \
            "$OUT/matrix/run.rs" -o "$OUT/matrixrun"
        shift || true
        exec "$OUT/matrixrun" "${1:-$REPO/google_drive/clients}" ;;
    instance) toolchain; sysroot; deps; real_deps
        # Run every instance folder under a client the way the viewer does, and
        # print one line per map: the ECWLD, its TRN2 extent, and the precinct
        # zones with their English names. This is the "does an instance map load
        # end to end" gate: the map file, the terrain header beside it, the
        # precinct file, the name table, and the zone centres the teleport list
        # needs.
        mkdir -p "$OUT/inst"
        cat > "$OUT/inst/run.rs" <<EOF
#[path = "$REPO/src/pck.rs"]
pub mod pck;
#[path = "$REPO/src/scene.rs"]
pub mod scene;
#[path = "$REPO/src/music.rs"]
pub mod music;
#[path = "$REPO/src/terrain_block.rs"]
pub mod terrain_block;
#[path = "$REPO/src/angelica_pck.rs"]
pub mod angelica_pck;
#[path = "$REPO/src/world_catalog.rs"]
pub mod world_catalog;
#[path = "$REPO/src/world_place_names.rs"]
pub mod world_place_names;
use std::path::{Path, PathBuf};
fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("client root (maps/)"));
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root).expect("read maps dir")
        .flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    let (mut maps, mut bad) = (0, 0);
    println!("{:<6} {:<7} {:>7} {:>9} {:>7} {:>7} {:>6}  {}", "map", "ecwld", "blocks", "extent", "zones", "centres", "tracks", "english names");
    for dir in dirs {
        let id = dir.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
        let row = |what: &str, message: String| {
            println!("{id:<6} {what:<7} {message}");
        };
        let ecwld = dir.join(format!("{id}.ecwld"));
        let trn2 = dir.join(format!("{id}.trn2"));
        if !ecwld.is_file() { continue; }
        let scene = match scene::Scene::load(&ecwld) {
            Ok(s) => s,
            Err(e) => { row("FAIL", format!("ECWLD: {e}")); bad += 1; continue; }
        };
        maps += 1;
        let blocks = scene.blocks.len();
        let extent = match std::fs::read(&trn2).ok().and_then(|b| terrain_block::trn2_header(&b).ok()) {
            Some(h) => format!("{:.0}x{:.0}", h.width, h.length),
            None => "no trn2".to_string(),
        };
        let (zones, centres, tracks, names) = match music::Zones::open(&dir.join("precinct.clt")) {
            Ok(z) => {
                let centres = z.zones.iter().filter(|z| z.center.is_some()).count();
                let tracks: usize = z.zones.iter().map(|z| z.tracks.len()).sum();
                // Two distinct English names are enough to show the table
                // answered; printing all of them would bury the row.
                let mut english = Vec::new();
                for zone in &z.zones {
                    let name = world_place_names::english(zone.name.trim());
                    if !english.iter().any(|n: &String| n == name) {
                        english.push(name.to_string());
                    }
                }
                english.truncate(2);
                (z.zones.len(), centres, tracks, english.join(" / "))
            }
            Err(e) => (0, 0, 0, format!("precinct FAILED: {e}")),
        };
        println!("{id:<6} {:<7} {blocks:>7} {extent:>9} {zones:>7} {centres:>7} {tracks:>6}  {names}",
            if blocks == 0 { "empty" } else { "v" });
    }
    eprintln!("{maps} instance map(s) read, {bad} failed");
    std::process::exit(if bad == 0 { 0 } else { 1 });
}
EOF
        "$RUSTC" --edition 2021 --crate-name instancecheck --sysroot "$SYSROOT" \
            -L "$OUT" --extern flate2="$OUT/libflate2.rlib" \
            --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            --extern image="$OUT/libimage_stub.rlib" \
            -o "$OUT/instance-check" "$OUT/inst/run.rs"
        shift || true
        (cd "$REPO" && exec "$OUT/instance-check" "${1:-$REPO/google_drive/clients/element/maps}") ;;
    matrix-maps) toolchain; sysroot; deps; real_deps
        # One line per map per client, which is what
        # `docs/cross-version-matrix.md` records. Same reader as the viewer; a
        # map that fails prints the error and the exit status is non-zero.
        mkdir -p "$OUT/mm"
        cat > "$OUT/mm/run.rs" <<EOF
#[path = "$REPO/src/pck.rs"]
pub mod pck;
#[path = "$REPO/src/scene.rs"]
pub mod scene;
#[path = "$REPO/src/music.rs"]
pub mod music;
#[path = "$REPO/src/terrain_block.rs"]
pub mod terrain_block;
use std::path::PathBuf;
fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("clients dir"));
    let mut clients: Vec<PathBuf> = std::fs::read_dir(&root).expect("read clients dir")
        .flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    clients.sort();
    println!("| client | map | blocks | grid | tree types | trees | water refs | grass | ornaments | effects | models | sounds | precinct |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    let mut failed = 0;
    for client in &clients {
        let name = client.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
        let element = if client.join("element").is_dir() { client.join("element") } else { client.clone() };
        let maps = element.join("maps");
        let mut dirs: Vec<PathBuf> = match std::fs::read_dir(&maps) {
            Ok(rd) => rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect(),
            Err(_) => continue,
        };
        dirs.sort();
        for dir in dirs {
            let id = dir.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
            let path = dir.join(format!("{id}.ecwld"));
            if !path.is_file() { continue; }
            match scene::Scene::load(&path) {
                Ok(s) => {
                    let blocks = s.blocks.len();
                    let grid = s.blocks.first().map(|_| format!("{}x{}", s.rows, s.cols)).unwrap_or_default();
                    let sum = |f: &dyn Fn(&scene::Block) -> usize| s.blocks.iter().map(f).sum::<usize>();
                    let precinct = match music::Zones::open(&dir.join("precinct.clt")) {
                        Ok(z) => format!("{} zones", z.zones.len()),
                        Err(e) => format!("FAILED: {e}"),
                    };
                    println!("| {name} | {id} | {blocks} | {grid} | {} | {} | {} | {} | {} | {} | {} | {} | {precinct} |",
                        s.tree_types.len(),
                        sum(&|b| b.trees.len()),
                        sum(&|b| b.water.len()),
                        sum(&|b| b.grass.len()),
                        sum(&|b| b.ornaments.len()),
                        sum(&|b| b.effects.len()),
                        sum(&|b| b.models.len()),
                        sum(&|b| b.sounds.len()));
                }
                Err(e) => { println!("| {name} | {id} | FAILED: {e} | | | | | | | | | | |"); failed += 1; }
            }
        }
    }
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
EOF
        "$RUSTC" --edition 2021 --crate-name matrixmaps --sysroot "$SYSROOT" \
            -L "$OUT" --extern flate2="$OUT/libflate2.rlib" \
            --extern encoding_rs="$OUT/libencoding_rs.rlib" \
            --extern image="$OUT/libimage_stub.rlib" \
            -o "$OUT/matrix-maps" "$OUT/mm/run.rs"
        shift || true
        (cd "$REPO" && exec "$OUT/matrix-maps" "${1:-$REPO/google_drive/clients}") ;;
    *) echo "usage: $0 {setup|check|test|fmt|client|water|gfx|music|material|scene|pck|content|matrix|matrix-maps|instance}" >&2; exit 2 ;;esac
