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
    let text = encoding_rs::GBK
        .decode(&b[..end])
        .0
        .replace("\r\n", "\n")
        .replace('\r', "\n");
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
    let mut meshes = Vec::new();
    for _ in 0..count {
        let st = find(b, q, b"MESH: ")?;
        let (_, p) = cstr(b, st)?;
        let nv = u(b, p)? as usize;
        let ni = u(b, p + 4)? as usize;
        let nf = u(b, p + 8)? as usize;
        if nv == 0 || nv > 100_000 || ni == 0 || ni > 300_000 || ni % 3 != 0 || nf != 1 {
            return Err("unsupported MOXB mesh".into());
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
        let t = decoded.replace('\\', "/").to_lowercase();
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
impl Grass {
    pub fn load(scene: &scene::Scene, dir: &Path, element: &Path) -> Result<Self, String> {
        let b = fs::read(ecbsd_path(dir)).map_err(|e| e.to_string())?;
        if u(&b, 0)? != 0x42584f4d || u(&b, 4)? != 0x42534446 || u(&b, 8)? != 12 {
            return Err("unsupported grass EC-BSD".into());
        }
        let n = u(&b, 12)? as usize;
        if n > 10000 {
            return Err("invalid grass BSData count".into());
        }
        let mut p = 0x4c;
        let mut names = Vec::new();
        for _ in 0..n {
            names.push(water::path_string(&b, &mut p)?)
        }
        let mut records = Vec::new();
        let mut seen = HashMap::new();
        for block in &scene.blocks {
            for r in &block.grass {
                if let Some(previous) = seen.insert(r.export_id, r.offset) {
                    if previous != r.offset {
                        return Err("inconsistent grass record".into());
                    }
                    continue;
                }
                let off = r.offset as usize;
                records.push((u(&b, off + 28)? as usize, records.len(), off));
            }
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
        let process_group = |&(index, group): &(usize, &[(usize, usize, usize)])| {
            let name = names
                .get(index)
                .ok_or_else(|| "grass BSData index".to_string())?;
            let path = water::resolve_case_insensitive(dir, name).map_err(|e| e.to_string())?;
            let data = fs::read(path).map_err(|e| e.to_string())?;
            if u(&data, 0).map_err(|e| e.to_string())? != 0x42584f4d
                || u(&data, 4).map_err(|e| e.to_string())? != 2
            {
                return Err("unsupported grass BSData".to_string());
            }
            let tail = data
                .len()
                .checked_sub(4)
                .ok_or_else(|| "short grass BSData".to_string())?;
            let count = u(&data, tail).map_err(|e| e.to_string())? as usize;
            let table_len = count
                .checked_mul(4)
                .ok_or_else(|| "grass table overflow".to_string())?;
            let table = tail
                .checked_sub(table_len)
                .ok_or_else(|| "bad grass table".to_string())?;
            let mut group_areas = Vec::with_capacity(group.len());
            for &(_, order, off) in group {
                let type_id = u(&b, off).map_err(|e| e.to_string())? as usize;
                if type_id >= scene.grass_types.len() {
                    return Err("grass type index".to_string());
                }
                let cx = f(&b, off + 4).map_err(|e| e.to_string())?;
                let cy = f(&b, off + 8).map_err(|e| e.to_string())?;
                let cz = f(&b, off + 12).map_err(|e| e.to_string())?;
                let center = [cx, cy, cz];
                let grid = f(&b, off + 16).map_err(|e| e.to_string())?;
                let width = u(&b, off + 20).map_err(|e| e.to_string())? as usize;
                let height = u(&b, off + 24).map_err(|e| e.to_string())? as usize;
                if width == 0
                    || height == 0
                    || width > 4096
                    || height > 4096
                    || width * height % 8 != 0
                {
                    return Err("grass mask size".to_string());
                }
                let cells = width
                    .checked_mul(height)
                    .ok_or_else(|| "grass mask overflow".to_string())?;
                let bytes = cells / 8;
                let rec = u(&b, off + 32).map_err(|e| e.to_string())? as usize;
                if rec >= count {
                    return Err("grass record index".to_string());
                }
                let start = u(&data, table + rec * 4).map_err(|e| e.to_string())? as usize;
                let stored = u(&data, start).map_err(|e| e.to_string())? as usize;
                let raw = u(&data, start + 4).map_err(|e| e.to_string())? as usize;
                if raw != bytes || stored > bytes * 2 + 1024 {
                    return Err("invalid grass compressed size".to_string());
                }
                let src = data
                    .get(start + 8..start + 8 + stored)
                    .ok_or_else(|| "short grass compressed bitmap".to_string())?;
                let output = crate::pck::inflate(src, bytes)?;
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
        let entries: HashMap<_, _> = pck::directory(&mut archive)?
            .into_iter()
            .map(|e| (e.name.to_lowercase(), e))
            .collect();
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
                        let entries = &entries;
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
                                    match entries
                                        .get(&name)
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
                                    match entries.get(&name) {
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
        for m in models.iter().flatten() {
            for mesh in &m.meshes {
                if !textures.contains_key(&mesh.texture) {
                    let alt = mesh.texture.replace("/textures/", "/");
                    // One retail asset names its DDS with a space while the
                    // matching archive member uses an underscore.
                    let spaced = mesh.texture.replace(' ', "_");
                    let entry = entries
                        .get(&mesh.texture)
                        .or_else(|| entries.get(&alt))
                        .or_else(|| entries.get(&spaced));
                    if let Some(entry) = entry {
                        if let Ok(bytes) = pck::read_entry(&mut archive, entry) {
                            if let Ok(img) = image::load_from_memory(&bytes) {
                                textures.insert(mesh.texture.clone(), img.into_rgba8());
                                match crate::vegetation_textures::authored_lower_mips(&bytes) {
                                    Ok(mips) => {
                                        texture_mips.insert(mesh.texture.clone(), mips);
                                    }
                                    Err(e) => eprintln!(
                                        "Grass texture {} authored mips unavailable: {e}",
                                        mesh.texture
                                    ),
                                }
                                continue;
                            }
                        }
                    }
                    warnings.insert(mesh.texture.clone());
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
