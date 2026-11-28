//! Experimental decoding of client grass placement masks and MOXB geometry.
//! EC-BSD loader and MSB-first bit order traced in elementclient.exe.
#[path = "grass_random.rs"]
pub mod random;
use crate::{pck, scene, water};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    path::{Path, PathBuf},
};

/// Instance maps name their EC-BSD after the map folder (`a01.ecbsd`), while
/// the main world and stand-alone fixtures keep `world.ecbsd`. This local
/// resolver keeps standalone diagnostic binaries independent of the viewer's
/// world catalog module.
fn ecbsd_path(dir: &Path) -> PathBuf {
    let stem = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("world");
    let named = dir.join(format!("{stem}.ecbsd"));
    if named.is_file() {
        named
    } else {
        dir.join("world.ecbsd")
    }
}

#[derive(Debug)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}
#[derive(Debug)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub texture: String,
}
#[derive(Debug)]
pub struct Model {
    pub meshes: Vec<Mesh>,
}
/// Retain the original compressed source instead of IDs and eager transforms.
pub struct ActiveMask {
    packed: Vec<u8>,
    count: usize,
}
impl ActiveMask {
    pub fn len(&self) -> usize {
        self.count
    }
}
pub struct Area {
    pub type_id: usize,
    pub center: [f32; 3],
    pub grid: f32,
    pub width: usize,
    pub height: usize,
    pub active: ActiveMask,
}
impl Area {
    pub fn placements(
        &self,
        typ: &scene::GrassType,
    ) -> Result<random::PlacementIter<'static>, String> {
        let size = self.width * self.height / 8;
        let bits = crate::pck::inflate(&self.active.packed, size)?;
        Ok(random::PlacementIter::owned(
            self.width,
            self.height,
            bits,
            typ.size,
            typ.size_variation,
        ))
    }
}
pub struct Grass {
    pub areas: Vec<Area>,
    pub types: Vec<scene::GrassType>,
    pub models: Vec<Option<Model>>,
    pub textures: HashMap<String, image::RgbaImage>,
    /// Authored lower mip levels keyed by the same texture path as `textures`.
    /// The default world path ignores these; P4.07 opt-in uploads them verbatim.
    pub texture_mips: HashMap<String, Vec<image::RgbaImage>>,
}
impl Grass {
    /// A map with no grass, for a map whose placement file cannot be read.
    ///
    /// The types still come from the scene, so the rest of the renderer sees
    /// the same shape it always does; only `areas` is empty.
    pub fn empty(scene: &scene::Scene) -> Self {
        Self {
            areas: Vec::new(),
            types: scene.grass_types.clone(),
            models: Vec::new(),
            textures: HashMap::new(),
            texture_mips: HashMap::new(),
        }
    }
}
/// Record one dropped placement without letting a bad area take the map down.
fn note(sink: &std::sync::Mutex<Vec<String>>, message: String) {
    if let Ok(mut notes) = sink.lock() {
        notes.push(message);
    }
}

/// Bounds on one MOXB mesh, from the 138 in the staged `grasses.pck`.
///
/// Measured maxima there are 56 vertices and 144 indices; these are set high
/// enough that a genuine larger mesh still reads, and low enough that a damaged
/// count cannot allocate a gigabyte.
const MAX_MOXB_VERTICES: usize = 100_000;
const MAX_MOXB_INDICES: usize = 300_000;

/// Bound on the names a grass EC-BSD placement file declares.
///
/// Measured over 8 real placement files: the maximum is **88**
/// (`world.ecbsd`, 2,678,024 B). The rest are far smaller -- `a91` 12, `a85`
/// 9, `a64` and `login` 4, `a117`/`a79`/`a95` 1. The staged *packs* contain no
/// EC-BSD at all; these files live under `maps/world/` in a full client, which
/// is why the earlier note said the maximum was unavailable.
///
/// So the limit sits two orders of magnitude above the largest real table and
/// is a guard against a damaged dword, not a statement about the format.
const ECBSD_NAME_LIMIT: usize = 10_000;

fn cstr<'a>(b: &'a [u8], start: usize) -> Result<(&'a [u8], usize), String> {
    let bytes = b.get(start..).ok_or("invalid MOXB string")?;
    let n = bytes
        .iter()
        .position(|x| *x == 0)
        .ok_or("unterminated MOXB string")?;
    if n > 4096 {
        return Err("oversized MOXB string".into());
    }
    Ok((&bytes[..n], start + n + 1))
}
fn find(b: &[u8], p: usize, s: &[u8]) -> Result<usize, String> {
    let data = b.get(p..).ok_or("bad MOXB offset")?;
    data.windows(s.len())
        .position(|w| w == s)
        .map(|n| n + p)
        .ok_or_else(|| "missing MOXB marker".into())
}
fn u(b: &[u8], p: usize) -> Result<u32, String> {
    water::u(b, p)
}
fn f(b: &[u8], p: usize) -> Result<f32, String> {
    water::f(b, p)
}
/// The `.mod` members of `grasses.pck` carry no geometry. Each is a small text
/// container that names the real `.mox` child:
///
/// ```text
/// MOXTMODEL: <name>
/// {
/// CHILDFRAME: 1
/// {
/// <name>.mox
/// }
/// CHILDMODEL: 0
/// ACTIONS: 0
/// GROUPACTIONS: 0
/// }
/// GFXEVENT: 0
/// SFXEVENT: 0
/// USER_PROPERTY DO NOT EDIT
/// ```
///
/// All 144 in the retail archive have exactly this shape: one `CHILDFRAME`
/// child and every other counter zero. This parser accepts only that shape, so a
/// richer container fails loudly rather than being half-read, and it returns the
/// child name for the caller to resolve inside the same pack and directory.
pub fn mox_wrapper_child(b: &[u8]) -> Result<String, String> {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    let decoded = encoding_rs::GBK.decode(&b[..end]).0;
    let text = if decoded.contains('\r') {
        decoded.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        decoded.into_owned()
    };
    if !text.starts_with("MOXTMODEL: ") {
        return Err("unsupported MOXTMODEL header".into());
    }
    let line_value = |key: &str| -> Option<String> {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_string()))
    };
    for (key, want) in [
        ("CHILDFRAME: ", "1"),
        ("CHILDMODEL: ", "0"),
        ("ACTIONS: ", "0"),
        ("GROUPACTIONS: ", "0"),
        ("GFXEVENT: ", "0"),
        ("SFXEVENT: ", "0"),
    ] {
        match line_value(key) {
            // CHILDFRAME's value names the child count in the block that follows.
            Some(v) if v == want => {}
            Some(v) => return Err(format!("unsupported MOXTMODEL {key}{v}")),
            None => return Err(format!("missing MOXTMODEL {key}")),
        }
    }
    // The child name sits inside `CHILDFRAME`'s own braces, i.e. at depth 2:
    //   MOXTMODEL: x   {   CHILDFRAME: 1   {   x.mox   }   CHILDMODEL: 0 ...
    // Reading at depth 1 instead would pick up `CHILDMODEL: 0` as the name.
    let mut depth = 0i32;
    let mut in_childframe = false;
    for line in text.lines() {
        let l = line.trim();
        if l == "{" {
            depth += 1;
        } else if l == "}" {
            depth -= 1;
        } else if l.starts_with("CHILDFRAME") {
            in_childframe = true;
        } else if in_childframe && depth == 2 && !l.is_empty() {
            let child = l.to_lowercase();
            if !child.ends_with(".mox")
                || child.contains("..")
                || child.contains('/')
                || child.contains('\\')
            {
                return Err("unsafe MOXTMODEL child name".into());
            }
            return Ok(child);
        }
    }
    Err("MOXTMODEL names no child".into())
}
fn parse_model(b: &[u8], name: &str) -> Result<Model, String> {
    if !b.starts_with(b"MOXB") {
        return Err("unsupported MOXB header".into());
    }
    let mut frame = 4;
    if b.get(frame..).is_some_and(|s| s.starts_with(b"VERSION: ")) {
        let (version, end) = cstr(b, frame)?;
        if version != b"VERSION: 2" {
            return Err("unsupported MOXB version".into());
        }
        frame = end;
    }
    if !b.get(frame..).is_some_and(|s| s.starts_with(b"FRAME: ")) {
        return Err("missing MOXB frame".into());
    }
    let (_, mut q) = cstr(b, frame)?;
    let frames = u(b, q)? as usize;
    q += 4;
    if frames != 1 {
        return Err("animated MOXB frames unsupported".into());
    }
    q += 64;
    let children = u(b, q)?;
    let count = u(b, q + 4)? as usize;
    q += 8;
    if children != 0 || count == 0 || count > 64 {
        return Err("unsupported MOXB hierarchy".into());
    }
    let dir = name.rsplit_once('/').map(|x| x.0).unwrap_or("");
    let mut meshes = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let st = find(b, q, b"MESH: ")?;
        let (_, p) = cstr(b, st)?;
        let nv = u(b, p)? as usize;
        let ni = u(b, p + 4)? as usize;
        let nf = u(b, p + 8)? as usize;
        // Measured over all 138 MOXB meshes in the staged `grasses.pck`: at most
        // 56 vertices and 144 indices, and `nf` is exactly 1 in every one. The
        // caps sit three orders of magnitude above those and exist to stop a
        // damaged count from reserving memory. `nf != 1` is a different kind of
        // check -- the mesh is one triangle-strip frame, and a file declaring
        // more is a layout this reader has not been shown -- so it stays an
        // equality and says so by name rather than joining the caps.
        if nv == 0 || nv > MAX_MOXB_VERTICES || ni == 0 || ni > MAX_MOXB_INDICES || ni % 3 != 0 {
            return Err(format!(
                "unsupported MOXB mesh: {nv} vertices (limit {MAX_MOXB_VERTICES}), \
                 {ni} indices (limit {MAX_MOXB_INDICES})"
            ));
        }
        if nf != 1 {
            return Err(format!(
                "unsupported MOXB frame count {nf}; this build reads the one-frame layout"
            ));
        }
        let tex = find(b, p, b"TEXTURE: ")?;
        let vertbytes = nv.checked_mul(32).ok_or("MOXB vertex overflow")?;
        let indbytes = ni.checked_mul(2).ok_or("MOXB index overflow")?;
        let inds = tex
            .checked_sub(vertbytes + indbytes)
            .ok_or("short MOXB mesh")?;
        if inds < p + 18 {
            return Err("bad MOXB mesh offsets".into());
        }
        let mut indices = Vec::with_capacity(ni);
        for i in 0..ni {
            let k = inds + i * 2;
            let value = u16::from_le_bytes(
                b.get(k..k + 2)
                    .ok_or("short MOXB index")?
                    .try_into()
                    .unwrap(),
            ) as u32;
            if value as usize >= nv {
                return Err("MOXB index out of range".into());
            }
            indices.push(value)
        }
        let mut vertices = Vec::with_capacity(nv);
        let base = inds + indbytes;
        for i in 0..nv {
            let p = base + i * 32;
            vertices.push(Vertex {
                pos: [f(b, p)?, f(b, p + 4)?, f(b, p + 8)?],
                normal: [f(b, p + 12)?, f(b, p + 16)?, f(b, p + 20)?],
                uv: [f(b, p + 24)?, f(b, p + 28)?],
            })
        }
        let (t, end) = cstr(b, tex + 9)?;
        let (decoded, _, bad) = encoding_rs::GBK.decode(t);
        if bad {
            return Err("invalid MOXB texture path".into());
        }
        let t = if decoded.is_ascii() {
            let mut s = String::with_capacity(decoded.len());
            for b in decoded.bytes() {
                s.push(if b == b'\\' {
                    '/'
                } else {
                    (b as char).to_ascii_lowercase()
                });
            }
            s
        } else {
            let mut s = decoded.to_lowercase();
            if s.as_bytes().contains(&b'\\') {
                unsafe {
                    for b in s.as_bytes_mut() {
                        if *b == b'\\' {
                            *b = b'/';
                        }
                    }
                }
            }
            s
        };
        if t.contains("..") || t.starts_with('/') || t.contains(':') {
            return Err("unsafe MOXB texture path".into());
        }
        let texture = format!("{dir}/textures/{t}");
        meshes.push(Mesh {
            vertices,
            indices,
            texture,
        });
        q = end;
    }
    Ok(Model { meshes })
}
#[cfg(test)]
fn mask(b: &[u8], expected: usize) -> Result<Vec<u32>, String> {
    if b.len() != expected {
        return Err("bad grass bitmap length".into());
    }
    let mut out = Vec::new();
    for (i, &byte) in b.iter().enumerate() {
        for k in 0..8 {
            if byte & (0x80 >> k) != 0 {
                out.push((i * 8 + k) as u32)
            }
        }
    }
    Ok(out)
}
/// Read the EC-BSD name table, or as much of it as the file really holds.
///
/// A map whose placement file is missing, truncated or not a grass EC-BSD at
/// all has no grass — but it is still a map. Returning `Err` here aborted the
/// whole load, which reached the user as a modal box holding one internal
/// string ("grass record index", "invalid grass compress size"). Nothing below
/// this point needs the name table to be complete, so the failures are returned
/// as notes and the caller decides what to keep.
fn grass_names(
    b: &[u8],
    notes: &std::sync::Mutex<Vec<String>>,
) -> Result<Vec<String>, &'static str> {
    if b.len() < 0x4c {
        return Err("the grass EC-BSD is shorter than its header");
    }
    // Same header check as the water and scenery readers: a revision other
    // than 12 is read, not refused, because the layout below the header has
    // not changed between the shipped revisions.
    if crate::scene::ecbsd_header(b).is_err() {
        return Err("the EC-BSD is not a placement file this build reads");
    }
    let n = u(b, 12).unwrap_or(0) as usize;
    if n > ECBSD_NAME_LIMIT {
        // This function returns `&'static str`, so the bound is reported by
        // name rather than formatted. The largest real table is 88 names
        // (`world.ecbsd`); see `ECBSD_NAME_LIMIT`.
        return Err("the grass EC-BSD declares more names than a placement file can hold");
    }
    let mut p = 0x4c;
    let mut names = Vec::new();
    for index in 0..n {
        match water::path_string(b, &mut p) {
            Ok(name) => names.push(name),
            Err(_) => {
                note(
                    notes,
                    format!("grass EC-BSD name {index} of {n} is truncated"),
                );
                break;
            }
        }
    }
    Ok(names)
}

impl Grass {
    pub fn load(scene: &scene::Scene, dir: &Path, element: &Path) -> Result<Self, String> {
        // Diagnostics for placements that were dropped. Threads share one sink
        // and it is summarised once, so a map with many odd areas prints a
        // handful of lines instead of thousands. It has to exist before the
        // header is read, because the header can fail too.
        let notes: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let b = match fs::read(ecbsd_path(dir)) {
            Ok(b) => b,
            Err(error) => {
                // No placement file at all is a map without grass, not a
                // failure to open the map.
                crate::diag!(
                    TimingLoad,
                    "Grass: no EC-BSD placement file in this map ({error}); the map loads without grass"
                );
                return Ok(Self::empty(scene));
            }
        };
        let names = match grass_names(&b, &notes) {
            Ok(names) => names,
            Err(reason) => {
                crate::diag!(TimingLoad, "Grass: {reason}; the map loads without grass");
                return Ok(Self::empty(scene));
            }
        };
        let mut records = Vec::new();
        let mut seen = HashMap::new();
        let mut skipped_blocks = 0usize;
        for block in &scene.blocks {
            for r in &block.grass {
                if let Some(previous) = seen.insert(r.export_id, r.offset) {
                    if previous != r.offset {
                        // One export id with two offsets is a duplicated
                        // reference. Drop the copy instead of the map.
                        note(
                            &notes,
                            format!("grass export {} names two record offsets", r.export_id),
                        );
                        continue;
                    }
                    continue;
                }
                let off = r.offset as usize;
                // A placement offset past the end of the file costs this
                // reference and nothing else.
                match u(&b, off + 28) {
                    Ok(index) => records.push((index as usize, records.len(), off)),
                    Err(_) => {
                        skipped_blocks += 1;
                        if skipped_blocks <= 8 {
                            note(
                                &notes,
                                format!("grass area at {off}: outside the EC-BSD placement file"),
                            );
                        }
                    }
                }
            }
        }
        if skipped_blocks > 8 {
            note(
                &notes,
                format!(
                    "{} more grass areas outside the EC-BSD placement file",
                    skipped_blocks - 8
                ),
            );
        }
        // Read each BSData file exactly once, retaining only one at a time.
        // Restore original area order afterwards so draw order stays unchanged.
        records.sort_by_key(|r| r.0);

        let mut groups: Vec<(usize, &[(usize, usize, usize)])> = Vec::new();
        let mut g_start = 0usize;
        while g_start < records.len() {
            let idx = records[g_start].0;
            let mut g_end = g_start + 1;
            while g_end < records.len() && records[g_end].0 == idx {
                g_end += 1;
            }
            groups.push((idx, &records[g_start..g_end]));
            g_start = g_end;
        }
        // A grass area that cannot be decoded is dropped on its own: the map
        // still draws every other patch. Only a placement file that cannot be
        // opened at all is reported once per group.
        let process_group = |&(index, group): &(usize, &[(usize, usize, usize)])| {
            let Some(name) = names.get(index) else {
                note(
                    &notes,
                    format!("BSData {index}: no name in the EC-BSD table"),
                );
                return Ok(Vec::new());
            };
            let path = match water::resolve_case_insensitive(dir, name) {
                Ok(path) => path,
                Err(_) => {
                    note(&notes, format!("{name}: not in this map folder"));
                    return Ok(Vec::new());
                }
            };
            let data = match fs::read(&path) {
                Ok(data) => data,
                Err(error) => {
                    note(&notes, format!("{name}: {error}"));
                    return Ok(Vec::new());
                }
            };
            // An authored-empty stub is a real file with a zero record count,
            // not corruption: nothing was placed in this area, so there is
            // nothing to decode.
            if data.len() >= 16 {
                let magic = u(&data, 0).unwrap_or(0);
                let version = u(&data, 4).unwrap_or(0);
                let count = u(&data, data.len() - 4).unwrap_or(u32::MAX);
                if magic == 0x42584f4d && version == 2 && count == 0 {
                    return Ok(Vec::new());
                }
            }
            if u(&data, 0).ok() != Some(0x42584f4d) || u(&data, 4).ok() != Some(2) {
                note(&notes, format!("{name}: unsupported grass BSData"));
                return Ok(Vec::new());
            }
            // Every bound below belongs to this one placement file. A file that
            // does not describe a reachable record table is skipped with a note
            // -- returning an error here is what turned one odd BSData file into
            // a modal box over the whole map.
            let Some(tail) = data.len().checked_sub(4) else {
                note(&notes, format!("{name}: shorter than its own trailer"));
                return Ok(Vec::new());
            };
            let count = u(&data, tail).unwrap_or(0) as usize;
            let Some(table) = count
                .checked_mul(4)
                .and_then(|table_len| tail.checked_sub(table_len))
            else {
                note(
                    &notes,
                    format!("{name}: record table of {count} does not fit the file"),
                );
                return Ok(Vec::new());
            };
            let mut group_areas = Vec::with_capacity(group.len());
            for &(_, order, off) in group {
                // Every per-area read is checked and, on failure, this one area
                // is dropped. Aborting here turned one odd area into a modal
                // error that blanked every grass patch in the map.
                let (Ok(type_id), Ok(cx), Ok(cy), Ok(cz)) =
                    (u(&b, off), f(&b, off + 4), f(&b, off + 8), f(&b, off + 12))
                else {
                    note(
                        &notes,
                        format!("grass area at {off}: past the placement file"),
                    );
                    continue;
                };
                let type_id = type_id as usize;
                if type_id >= scene.grass_types.len() {
                    note(&notes, format!("grass type {type_id} out of range"));
                    continue;
                }
                let center = [cx, cy, cz];
                let (Ok(grid), Ok(width), Ok(height)) =
                    (f(&b, off + 16), u(&b, off + 20), u(&b, off + 24))
                else {
                    note(&notes, format!("grass area at {off}: short record"));
                    continue;
                };
                let (width, height) = (width as usize, height as usize);
                if width == 0
                    || height == 0
                    || width > 4096
                    || height > 4096
                    || width * height % 8 != 0
                {
                    note(&notes, format!("mask {width}x{height}"));
                    continue;
                }
                let Some(cells) = width.checked_mul(height) else {
                    note(&notes, format!("grass area at {off}: mask size overflows"));
                    continue;
                };
                let bytes = cells / 8;
                let Ok(rec) = u(&b, off + 32) else {
                    note(&notes, format!("grass area at {off}: no record index"));
                    continue;
                };
                let rec = rec as usize;
                let Some(row) = rec
                    .checked_mul(4)
                    .and_then(|row| table.checked_add(row))
                    .map(|at| u(&data, at))
                else {
                    note(&notes, format!("area at {off}: record {rec}"));
                    continue;
                };
                let Ok(start) = row.map(|s| s as usize) else {
                    note(&notes, format!("area at {off}: record {rec}"));
                    continue;
                };
                if rec >= count {
                    note(&notes, format!("area at {off}: record {rec} of {count}"));
                    continue;
                }
                let (Ok(stored), Ok(raw)) = (u(&data, start), u(&data, start + 4)) else {
                    note(&notes, format!("area at {off}: record {rec} header"));
                    continue;
                };
                let (stored, raw) = (stored as usize, raw as usize);
                // The record's uncompressed size must be the area's own cell
                // count. A mismatch means the index points at a different
                // area's row, not that the map is unreadable.
                if raw != bytes || stored > bytes * 2 + 1024 {
                    note(
                        &notes,
                        format!(
                            "area at {off}: record {rec} holds {raw} bytes for a {bytes}-byte mask"
                        ),
                    );
                    continue;
                }
                let Some(src) = data.get(start + 8..start + 8 + stored) else {
                    note(&notes, format!("area at {off}: record {rec} is truncated"));
                    continue;
                };
                let Ok(output) = crate::pck::inflate(src, bytes) else {
                    note(
                        &notes,
                        format!("area at {off}: record {rec} does not inflate"),
                    );
                    continue;
                };
                let count = output.iter().map(|byte| byte.count_ones() as usize).sum();
                let active = ActiveMask {
                    packed: src.to_vec(),
                    count,
                };
                group_areas.push((
                    order,
                    Area {
                        type_id,
                        center,
                        grid,
                        width,
                        height,
                        active,
                    },
                ));
            }
            Ok::<_, String>(group_areas)
        };
        let bsd_workers = if groups.len() > 1 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .min(groups.len())
        } else {
            1
        };
        let bsd_results: Vec<Result<Vec<(usize, Area)>, String>> = if bsd_workers > 1 {
            let chunk_size = groups.len().div_ceil(bsd_workers);
            std::thread::scope(|s| {
                let handles: Vec<_> = groups
                    .chunks(chunk_size)
                    .map(|chunk| {
                        let process = &process_group;
                        s.spawn(move || chunk.iter().map(process).collect::<Vec<_>>())
                    })
                    .collect();
                handles
                    .into_iter()
                    .flat_map(|h| h.join().unwrap())
                    .collect()
            })
        } else {
            groups.iter().map(process_group).collect()
        };
        let mut areas = Vec::with_capacity(records.len());
        for res in bsd_results {
            areas.extend(res?);
        }
        areas.sort_by_key(|a| a.0);
        let areas: Vec<_> = areas.into_iter().map(|(_, area)| area).collect();
        let packed: usize = areas.iter().map(|a| a.active.packed.len()).sum();
        let active: usize = areas.iter().map(|a| a.active.len()).sum();
        eprintln!("Grass placement residency: {:.2} MiB compressed bitmaps; {} live cells; avoided {:.2} MiB of eager IDs/transforms", packed as f64 / 1048576., active,
            active as f64 * (4 + std::mem::size_of::<random::Placement>()) as f64 / 1048576.);
        let mut archive = File::open(element.join("grasses.pck")).map_err(|e| e.to_string())?;
        let (dir_entries, entry_index) = pck::normalized_index_arc(&mut archive)?;
        let mut models = Vec::with_capacity(scene.grass_types.len());
        let mut textures = HashMap::new();
        let mut texture_mips = HashMap::new();
        let mut warnings = HashSet::new();
        if !scene.grass_types.is_empty() {
            let workers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .min(scene.grass_types.len());
            let chunk_size = scene.grass_types.len().div_ceil(workers);
            let chunks: Vec<Vec<_>> = std::thread::scope(|s| {
                let handles: Vec<_> = scene
                    .grass_types
                    .chunks(chunk_size)
                    .map(|chunk| {
                        let dir_entries = &dir_entries;
                        let entry_index = &entry_index;
                        let archive = &archive;
                        s.spawn(move || {
                            chunk.iter().map(|t| {

                                // A grass type may name either the `.mox` geometry directly or the
                                // `.mod` text container that wraps it. The archive ships both (155
                                // `.mox` and 144 `.mod`); the `.mod` is a one-child manifest, so
                                // resolving it here keeps the rest of the loader unchanged.
                                let mut name = t.path.to_lowercase();
                                let mut skip = false;
                                if name.ends_with(".mod") {
                                    match entry_index
                                        .get(&name)
                                        .map(|&i| &dir_entries[i])
                                        .ok_or_else(|| "missing MOXTMODEL".to_string())
                                           .and_then(|e| {
                                               pck::read_entry_at(archive, e)
                                                   .map_err(|e| e.to_string())
                                            })
                                        .and_then(|b| mox_wrapper_child(&b))
                                    {
                                        Ok(child) => {
                                            let dir = name.rsplit_once('/').map(|x| x.0).unwrap_or("");
                                            name = format!("{dir}/{child}");
                                        }
                                        Err(error) => {
                                            eprintln!(
                                                "Unsupported grass model wrapper {name}: {error}"
                                            );
                                            skip = true;
                                        }
                                    }
                                }
                                if skip {
                                    None
                                } else {
                                    match entry_index.get(&name).map(|&i| &dir_entries[i]) {
                                        Some(e) => match pck::read_entry_at(archive, e)
                                            .map_err(|e| e.to_string())
                                            .and_then(|bytes| parse_model(&bytes, &name))
                                        {
                                            Ok(model) => Some(model),
                                            Err(error) => {
                                                eprintln!("Unsupported grass model {name}: {error}");
                                                None
                                            }
                                        },
                                        None => {
                                            eprintln!("Missing grass model {name}");
                                            None
                                        }
                                    }
                                }
                               })
                                    .collect::<Vec<_>>()
                            })
                        })
                        .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            for chunk in chunks {
                models.extend(chunk);
            }
        }
        let mut unique_tex: Vec<(String, Option<&pck::Entry>)> = Vec::new();
        let mut seen_tex: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for m in models.iter().flatten() {
            for mesh in &m.meshes {
                if !textures.contains_key(&mesh.texture) && seen_tex.insert(mesh.texture.as_str()) {
                    let alt = mesh.texture.replace("/textures/", "/");
                    let spaced = mesh.texture.replace(' ', "_");
                    let entry = entry_index
                        .get(&mesh.texture)
                        .or_else(|| entry_index.get(&alt))
                        .or_else(|| entry_index.get(&spaced))
                        .map(|&i| &dir_entries[i]);
                    unique_tex.push((mesh.texture.clone(), entry));
                }
            }
        }
        if !unique_tex.is_empty() {
            let tex_workers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .min(32)
                .min(unique_tex.len());
            let decode_one = |(tex_name, entry): &(String, Option<&pck::Entry>),
                              archive_ref: &File| {
                if let Some(entry) = entry {
                    if let Ok(bytes) = pck::read_entry_at(archive_ref, entry) {
                        if let Ok(img) = image::load_from_memory(&bytes) {
                            let mips = crate::vegetation_textures::authored_lower_mips(&bytes);
                            return (tex_name.clone(), Some((img.into_rgba8(), mips)));
                        }
                    }
                }
                (tex_name.clone(), None)
            };
            let decoded: Vec<_> = if tex_workers > 1 {
                let chunk_size = unique_tex.len().div_ceil(tex_workers);
                let archive_ref = &archive;
                std::thread::scope(|s| {
                    let handles: Vec<_> = unique_tex
                        .chunks(chunk_size)
                        .map(|chunk| {
                            s.spawn(move || {
                                chunk
                                    .iter()
                                    .map(|item| decode_one(item, archive_ref))
                                    .collect::<Vec<_>>()
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .flat_map(|h| h.join().unwrap())
                        .collect()
                })
            } else {
                unique_tex
                    .iter()
                    .map(|item| decode_one(item, &archive))
                    .collect()
            };
            for (tex_name, outcome) in decoded {
                if let Some((img, mips)) = outcome {
                    textures.insert(tex_name.clone(), img);
                    match mips {
                        Ok(mips) => {
                            texture_mips.insert(tex_name, mips);
                        }
                        Err(e) => {
                            eprintln!("Grass texture {tex_name} authored mips unavailable: {e}")
                        }
                    }
                } else {
                    warnings.insert(tex_name);
                }
            }
        }

        for path in &warnings {
            eprintln!("Missing or unsupported grass texture: {path}");
        }
        eprintln!("Loaded {} grass areas, {} active bits, {}/{} MOXB types, {} textures ({} texture paths unresolved)",areas.len(),areas.iter().map(|a|a.active.len()).sum::<usize>(),models.iter().filter(|x|x.is_some()).count(),models.len(),textures.len(),warnings.len());
        Ok(Self {
            areas,
            types: scene.grass_types.clone(),
            models,
            textures,
            texture_mips,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bits_are_most_significant_first() {
        assert_eq!(mask(&[0x81, 0x40], 2).unwrap(), vec![0, 7, 9]);
        assert!(mask(&[0], 2).is_err())
    }
    /// The largest real name table is 88 (`world.ecbsd`); versions 17 and 18
    /// both ship, and `ecbsd_header` gates on identity only, so both read.
    /// `ECBSD_NAME_LIMIT` must stay far above the measured maximum -- a cap
    /// that a real client trips is a refusal, not a guard.
    #[test]
    fn the_name_limit_is_above_every_measured_table() {
        let notes = std::sync::Mutex::new(Vec::new());
        for (version, count) in [(18u32, 88u32), (17, 1), (18, 12), (18, 4)] {
            let mut b = vec![0u8; 0x4c + 64 * count as usize];
            b[0..4].copy_from_slice(&0x42584f4du32.to_le_bytes());
            b[4..8].copy_from_slice(&0x42534446u32.to_le_bytes());
            b[8..12].copy_from_slice(&version.to_le_bytes());
            b[12..16].copy_from_slice(&count.to_le_bytes());
            let mut p = 0x4c;
            for i in 0..count {
                let name = format!("BSData/{}.dat", i + 1);
                b[p..p + 4].copy_from_slice(&(name.len() as u32).to_le_bytes());
                b[p + 4..p + 4 + name.len()].copy_from_slice(name.as_bytes());
                p += 4 + name.len();
            }
            let names = grass_names(&b[..p], &notes)
                .unwrap_or_else(|e| panic!("v{version} with {count} names must read, got {e}"));
            assert_eq!(names.len(), count as usize, "v{version} {count} names");
        }
        assert!((88usize) < ECBSD_NAME_LIMIT);
    }

    #[test]
    fn a_placement_file_that_is_not_grass_ec_bsd_costs_only_the_grass() {
        // The reported failure was a modal box holding "grass record index":
        // `Grass::load` returned `Err` from inside its per-group closure, and
        // the caller's `?` took the whole map down. A header the build does not
        // read is now a note plus an empty grass set, so this must be `Ok`.
        let notes = std::sync::Mutex::new(Vec::new());
        assert!(grass_names(b"", &notes).is_err());
        assert!(grass_names(b"not an ec-bsd file at all", &notes).is_err());
        // A well-formed header with a truncated name table keeps the names it
        // managed to read instead of failing.
        let mut b = Vec::new();
        b.extend_from_slice(&0x42584f4du32.to_le_bytes());
        b.extend_from_slice(&0x42534446u32.to_le_bytes());
        b.extend_from_slice(&12u32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.resize(0x4c, 0);
        // Names are a length word followed by GBK bytes, not NUL-terminated.
        b.extend_from_slice(&15u32.to_le_bytes());
        b.extend_from_slice(b"grasses/one.bsd");
        let names = grass_names(&b, &notes).unwrap();
        assert_eq!(names, vec!["grasses/one.bsd".to_string()], "got {names:?}");
        // A name whose length byte runs past the file is the truncation the
        // reported modal came from: the names that did read are kept.
        let mut cut = b.clone();
        cut.truncate(0x4c + 4 + 5);
        let kept = grass_names(&cut, &notes).unwrap();
        assert!(kept.is_empty(), "got {kept:?}");
    }
    #[test]
    fn truncated_mox_fails() {
        assert!(parse_model(b"MOXBFRAME: \0", "a/b").is_err());
    }
    #[test]
    fn parses_versioned_triangle_and_rejects_bad_index() {
        let mut b = b"MOXBVERSION: 2\0FRAME: \0".to_vec();
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 64]);
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(b"VERSION: 2\0MESH: \0");
        for v in [3u32, 3, 1] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&[0u8; 10]);
        let index = b.len();
        for v in [0u16, 1, 2] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for i in 0..3 {
            for v in [i as f32, 0., 0., 0., 1., 0., 0., 0.] {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        b.extend_from_slice(b"TEXTURE: x.dds\0");
        let m = parse_model(&b, "grasses/a/x.mox").unwrap();
        assert_eq!(m.meshes[0].indices, vec![0, 1, 2]);
        assert_eq!(m.meshes[0].vertices[2].pos[0], 2.);
        assert_eq!(m.meshes[0].texture, "grasses/a/textures/x.dds");
        b[index + 4..index + 6].copy_from_slice(&3u16.to_le_bytes());
        assert!(parse_model(&b, "grasses/a/x.mox").is_err());
    }

    /// A byte-for-byte member of the retail `grasses.pck`
    /// (`grasses/做好的花/荷叶01.mox`, 409 bytes, the smallest grass model in the
    /// 2019-05-03 archive). The synthetic test above builds its buffer to fit the
    /// parser, so it cannot catch a wrong vertex stride; this one cannot be
    /// wrong because nothing here was constructed - it is archive bytes.
    ///
    /// The payload is a 2x2 quad whose four vertices are exactly what the
    /// recovered layout predicts: all four normals are unit length and equal to
    /// (0,1,0), and the uvs are the four corners (0,1) (1,1) (0,0) (1,0). A
    /// wrong stride cannot produce unit-length normals at every one of 4,703
    /// vertices across all 155 archive models, which is how the 32-byte stride
    /// was confirmed rather than fitted.
    #[test]
    fn parses_a_retail_archive_grass_model() {
        let m = parse_model(REAL_MOX, "grasses/做好的花/荷叶01.mox").unwrap();
        let mesh = &m.meshes[0];
        assert_eq!(mesh.indices, vec![3, 1, 2, 0, 2, 1, 1, 0, 3, 2, 3, 0]);
        assert_eq!(mesh.vertices.len(), 4);
        for (i, v) in mesh.vertices.iter().enumerate() {
            assert_eq!(v.normal, [0., 1., 0.], "vertex {i} normal");
            let len_sq: f32 = v.normal.iter().map(|n| n * n).sum();
            assert!((len_sq - 1.).abs() < 1e-6, "vertex {i} normal not unit");
        }
        // The quad spans exactly 2.0 on both axes and lies flat. The stored
        // values are the nearest f32 to these decimals, so compare with a
        // tolerance rather than by equality.
        let all_f = |p: [f32; 3], e: [f32; 3]| (0..3).all(|i| (p[i] - e[i]).abs() < 1e-4);
        assert!(all_f(mesh.vertices[0].pos, [-0.8794, 0., -0.8346]));
        assert!(all_f(mesh.vertices[3].pos, [1.1206, 0., 1.1654]));
        // 2.0 x 2.0 footprint, no vertical extent.
        let dx = mesh.vertices[3].pos[0] - mesh.vertices[0].pos[0];
        let dz = mesh.vertices[3].pos[2] - mesh.vertices[0].pos[2];
        assert!(
            (dx - 2.).abs() < 1e-4 && (dz - 2.).abs() < 1e-4,
            "{dx} {dz}"
        );
        for v in &mesh.vertices {
            assert_eq!(v.pos[1], 0.);
        }
        // Corners, not interpolated junk.
        let uvs: Vec<[f32; 2]> = mesh.vertices.iter().map(|v| v.uv).collect();
        assert_eq!(uvs, vec![[0., 1.], [1., 1.], [0., 0.], [1., 0.]]);
        // GBK texture name resolved under the model's own directory.
        assert_eq!(mesh.texture, "grasses/做好的花/textures/荷叶01.dds");
    }
    const REAL_MOX: &[u8] = &[
        77, 79, 88, 66, 70, 82, 65, 77, 69, 58, 32, 0, 1, 0, 0, 0, 0, 0, 128, 63, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 63, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 128, 63, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 63, 0, 0, 0,
        0, 1, 0, 0, 0, 77, 69, 83, 72, 58, 32, 0, 4, 0, 0, 0, 12, 0, 0, 0, 1, 0, 0, 0, 0, 0, 40, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 0, 1, 0, 2, 0, 0, 0, 2, 0, 1, 0, 1, 0, 0, 0, 3, 0,
        2, 0, 3, 0, 0, 0, 96, 31, 97, 191, 0, 0, 0, 0, 91, 170, 85, 191, 0, 0, 0, 0, 0, 0, 128, 63,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 63, 80, 112, 143, 63, 0, 0, 0, 0, 91, 170, 85, 191, 0,
        0, 0, 0, 0, 0, 128, 63, 0, 0, 0, 0, 0, 0, 128, 63, 0, 0, 128, 63, 96, 31, 97, 191, 0, 0, 0,
        0, 210, 42, 149, 63, 0, 0, 0, 0, 0, 0, 128, 63, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 80,
        112, 143, 63, 0, 0, 0, 0, 210, 42, 149, 63, 0, 0, 0, 0, 0, 0, 128, 63, 0, 0, 0, 0, 0, 0,
        128, 63, 0, 0, 0, 0, 84, 69, 88, 84, 85, 82, 69, 58, 32, 186, 201, 210, 182, 48, 49, 46,
        100, 100, 115, 0, 77, 65, 84, 69, 82, 73, 65, 76, 58, 32, 0, 0, 0, 128, 63, 0, 0, 128, 63,
        0, 0, 128, 63, 0, 0, 128, 63, 0, 0, 128, 63, 0, 0, 128, 63, 0, 0, 128, 63, 0, 0, 128, 63,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 63, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 128, 63, 255, 255, 31, 65, 0, 0, 0, 0, 0,
    ];

    /// A real `.mod` member of `grasses.pck` (`grasses/水草/水草01.mod`), copied
    /// byte for byte. Its child name is GBK-encoded Chinese, which is the case
    /// that breaks a naive ASCII/UTF-8 walk.
    #[test]
    fn mox_wrapper_resolves_a_retail_mod_to_its_child() {
        assert_eq!(mox_wrapper_child(REAL_MOD).unwrap(), "水草01.mox");
    }
    /// The grammar is fixed and every archive member has exactly one child and
    /// all other counters zero; anything else must fail rather than half-read.
    #[test]
    fn mox_wrapper_rejects_containers_it_does_not_fully_understand() {
        let good = "MOXTMODEL: a\r\n{\r\nCHILDFRAME: 1\r\n{\r\na.mox\r\n}\r\n\
                    CHILDMODEL: 0\r\nACTIONS: 0\r\nGROUPACTIONS: 0\r\n}\r\n\
                    GFXEVENT: 0\r\nSFXEVENT: 0\r\nUSER_PROPERTY DO NOT EDIT\r\n";
        assert_eq!(mox_wrapper_child(good.as_bytes()).unwrap(), "a.mox");
        // Two children: not a shape the client ships, refuse it.
        assert!(
            mox_wrapper_child(good.replace("CHILDFRAME: 1", "CHILDFRAME: 2").as_bytes()).is_err()
        );
        // A nested model or actions table means the wrapper is not a pure
        // manifest, so the single-child assumption no longer holds.
        assert!(
            mox_wrapper_child(good.replace("CHILDMODEL: 0", "CHILDMODEL: 1").as_bytes()).is_err()
        );
        assert!(mox_wrapper_child(good.replace("ACTIONS: 0", "ACTIONS: 3").as_bytes()).is_err());
        // Path traversal and subdirectories are not valid child names.
        assert!(mox_wrapper_child(good.replace("a.mox", "../a.mox").as_bytes()).is_err());
        assert!(mox_wrapper_child(good.replace("a.mox", "sub/a.mox").as_bytes()).is_err());
        // Wrong container entirely.
        assert!(mox_wrapper_child(b"MOXBVERSION: 2\0").is_err());
        assert!(mox_wrapper_child(b"").is_err());
    }
    const REAL_MOD: &[u8] = &[
        77, 79, 88, 84, 77, 79, 68, 69, 76, 58, 32, 203, 174, 178, 221, 48, 49, 13, 10, 123, 13,
        10, 67, 72, 73, 76, 68, 70, 82, 65, 77, 69, 58, 32, 49, 13, 10, 123, 13, 10, 203, 174, 178,
        221, 48, 49, 46, 109, 111, 120, 13, 10, 125, 13, 10, 67, 72, 73, 76, 68, 77, 79, 68, 69,
        76, 58, 32, 48, 13, 10, 65, 67, 84, 73, 79, 78, 83, 58, 32, 48, 13, 10, 71, 82, 79, 85, 80,
        65, 67, 84, 73, 79, 78, 83, 58, 32, 48, 13, 10, 125, 13, 10, 71, 70, 88, 69, 86, 69, 78,
        84, 58, 32, 48, 13, 10, 83, 70, 88, 69, 86, 69, 78, 84, 58, 32, 48, 13, 10, 85, 83, 69, 82,
        95, 80, 82, 79, 80, 69, 82, 84, 89, 32, 68, 79, 32, 78, 79, 84, 32, 69, 68, 73, 84, 13, 10,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0,
    ];
}
