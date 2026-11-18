//! Audit real EC-BSD patches and one BSData file, without a complete client.
#![allow(dead_code)]
// `water.rs`, `far_trees_gpu.rs` and the grass loader report through
// `crate::diag!`; a `#[path]` module set is its own crate root, so the stub
// supplies it. See `support/diag_stub.rs`.
#[macro_use]
#[path = "support/diag_stub.rs"]
mod diag_stub;
#[allow(unused_imports)]
pub use diag_stub::diagnostics;

#[path = "../pck.rs"]
mod pck;
#[path = "../grass_random.rs"]
mod random;
#[path = "../scene.rs"]
mod scene;
#[path = "../water.rs"]
mod water;
use std::{
    collections::{BTreeMap, HashSet},
    io::Read,
    path::Path,
};
fn u(b: &[u8], p: usize) -> usize {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap()) as usize
}
fn f(b: &[u8], p: usize) -> f32 {
    f32::from_bits(u(b, p) as u32)
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let world = scene::Scene::load(Path::new(&args[1])).unwrap();
    let b = std::fs::read(&args[2]).unwrap();
    let dat = std::fs::read(&args[3]).unwrap();
    let basename = Path::new(&args[3])
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_lowercase();
    let mut p = 0x4c;
    let names: Vec<_> = (0..u(&b, 12))
        .map(|_| water::path_string(&b, &mut p).unwrap())
        .collect();
    let mut seen = HashSet::new();
    let mut dims = BTreeMap::new();
    let mut total = 0;
    let mut selected = 0;
    let mut active_count = 0;
    let mut samples = Vec::new();
    for block in &world.blocks {
        for r in &block.grass {
            if !seen.insert(r.export_id) {
                continue;
            }
            let off = r.offset as usize;
            let width = u(&b, off + 20);
            let height = u(&b, off + 24);
            *dims.entry((width % 16, height % 16)).or_insert(0) += 1;
            total += 1;
            let ix = u(&b, off + 28);
            if names[ix]
                .replace('\\', "/")
                .to_lowercase()
                .rsplit('/')
                .next()
                != Some(&basename)
            {
                continue;
            }
            let rec = u(&b, off + 32);
            let table = dat.len() - 4 - u(&dat, dat.len() - 4) * 4;
            let start = u(&dat, table + rec * 4);
            let stored = u(&dat, start);
            let mut mask = Vec::new();
            flate2::read::ZlibDecoder::new(&dat[start + 8..start + 8 + stored])
                .read_to_end(&mut mask)
                .unwrap();
            let active: Vec<_> = mask
                .iter()
                .enumerate()
                .flat_map(|(i, &v)| {
                    (0..8)
                        .filter(move |k| v & (0x80 >> k) != 0)
                        .map(move |k| (i * 8 + k) as u32)
                })
                .collect();
            let typ = &world.grass_types[u(&b, off)];
            let out = random::placements(width, height, &active, typ.size, typ.size_variation);
            assert_eq!(out.len(), active.len());
            assert!(out.iter().all(|x| x.scale.is_finite()
                && x.yaw.is_finite()
                && x.col < width as u32
                && x.row < height as u32));
            if samples.len() < 3 {
                samples.push((
                    width,
                    height,
                    active.len(),
                    f(&b, off + 16),
                    out.first().copied(),
                ));
            }
            selected += 1;
            active_count += active.len();
        }
    }
    println!("ECWLD areas={total}; dimensions modulo 16={dims:?}; file={basename} areas={selected}, active cells={active_count}; sample={samples:?}");
    assert!(selected > 0);
}
