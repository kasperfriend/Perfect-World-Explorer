//! rustc --edition=2021 tools/validate-terrain-lightmaps.rs -o /tmp/validate-lightmaps
//! validate-lightmaps /path/to/element/maps/world
//! Missing files are reported as incomplete, never as a successful census.
#![allow(dead_code)]
#[path = "../src/model_dds.rs"]
mod model_dds;
#[path = "../src/terrain_lightmaps.rs"]
mod terrain_lightmaps;
use std::{fs, path::Path};
fn u(b: &[u8], p: usize) -> Result<usize, String> {
    Ok(u32::from_le_bytes(b.get(p..p + 4).ok_or("truncated TRN2")?.try_into().unwrap()) as usize)
}
fn check_record(bytes: &[u8]) -> Result<(usize, usize), String> {
    let t = terrain_lightmaps::Tiles::parse(bytes)?;
    for tile in 0..terrain_lightmaps::TILES {
        let d = model_dds::Dds::parse(t.tile(tile).unwrap())?;
        if d.format != model_dds::Format::Dxt1
            || d.mips.len() != 1
            || d.mips[0].width as usize != t.side()
            || d.mips[0].height as usize != t.side()
        {
            return Err(format!("unsupported or header-mismatched DDS tile {tile}"));
        }
    }
    Ok((terrain_lightmaps::TILES, t.side()))
}

fn run(dir: &Path) -> Result<(), String> {
    let b = fs::read(dir.join("world.trn2")).map_err(|e| e.to_string())?;
    if u(&b, 0)? != 0x54524e32 || u(&b, 4)? > 1 {
        return Err("unsupported TRN2".into());
    }
    let grid = u(&b, 20)?;
    let rows = u(&b, 28)?;
    let cols = u(&b, 32)?;
    let count = u(&b, 52)?;
    let mask = u(&b, 48)?;
    if grid == 0
        || grid > 256
        || rows == 0
        || rows > 4096
        || cols == 0
        || cols > 4096
        || count > 10000
        || mask == 0
        || (cols * grid) % mask != 0
        || (rows * grid) % mask != 0
    {
        return Err("invalid TRN2 geometry".into());
    }
    let x = cols * grid / mask;
    let y = rows * grid / mask;
    if u(&b, 40)? != x * y {
        return Err("TRN2 mask count mismatch".into());
    }
    let layout = terrain_lightmaps::Layout::new(x, x * y, count)?;
    let mut p = u(&b, 80)?
        .checked_add(4)
        .ok_or("TRN2 name offset overflow")?;
    let mut files = 0;
    let mut tiles = 0;
    let mut missing = 0;
    let mut bad = Vec::new();
    let mut seen = vec![false; x * y * 2];
    for sub in 0..count {
        let len = u(&b, p)?;
        p += 4;
        if len > 4096 {
            return Err("oversized subterrain name".into());
        }
        let name = std::str::from_utf8(b.get(p..p + len).ok_or("truncated subterrain name")?)
            .map_err(|e| e.to_string())?;
        p += len;
        if name.contains(['/', '\\']) || name == ".." {
            return Err("unsafe subterrain name".into());
        }
        for (which, ext) in ["t2lm", "t2lm1"].into_iter().enumerate() {
            let path = dir.join(format!("{name}.{ext}"));
            let bytes = match fs::read(&path) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    missing += 1;
                    continue;
                }
                Err(e) => {
                    bad.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            match check_record(&bytes) {
                Ok((n, _side)) => {
                    files += 1;
                    tiles += n;
                    for tile in 0..n {
                        let a = layout.area(sub, tile).unwrap() + which * x * y;
                        if seen[a] {
                            return Err("duplicate atlas destination".into());
                        }
                        seen[a] = true;
                    }
                }
                Err(e) => bad.push(format!("{}: {e}", path.display())),
            }
        }
    }
    println!("World: {x}x{y} mask areas; {count} subterrain pairs. Validated {files} files / {tiles} DDS tiles; missing {missing}; invalid {}.",bad.len());
    for e in &bad {
        eprintln!("{e}");
    }
    if missing != 0 || !bad.is_empty() || seen.iter().any(|x| !*x) {
        return Err("full-world lightmap census incomplete".into());
    }
    Ok(())
}
fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(dir) = args.next() else {
        eprintln!("usage: validate-lightmaps MAP_DIRECTORY | --records FILE [FILE ...]");
        std::process::exit(2);
    };
    let result = if dir == "--records" {
        let records: Vec<_> = args.collect();
        if records.is_empty() {
            Err("--records needs at least one file".into())
        } else {
            records.iter().try_for_each(|path| -> Result<(), String> {
                let bytes = fs::read(path).map_err(|e| e.to_string())?;
                let (count, side) = check_record(&bytes)?;
                println!(
                    "{}: {count} single-mip BC1 tiles, {side}x{side}, {} bytes",
                    Path::new(path).display(),
                    bytes.len()
                );
                Ok(())
            })
        }
    } else {
        run(Path::new(&dir))
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
