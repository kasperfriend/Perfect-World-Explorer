//! Client EC-BSD water references and A3DWaterArea v0x10002000/0x10003000 grids.
//! Record layout derived from the supplied elementclient.exe at 0x780d30 and 0x781760.
use crate::scene::Scene;
use std::{
    collections::HashMap,
    fs,
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

/// Authored shoreline control points and per-vertex day/night ARGB colors.
/// Version 0x10003000 serialization is read by client routine 0x782680.
#[derive(Debug, Clone)]
pub struct ShoreWave {
    pub capacity: usize,
    pub width: f32,
    pub points: Vec<[f32; 3]>,
    pub day_colors: Vec<u32>,
    pub night_colors: Vec<u32>,
}
#[derive(Debug, Clone)]
pub struct WaterArea {
    pub export_id: u32,
    pub color: u32,
    /// Selects the client's alternate simple-water area pass (record +44).
    pub simple: bool,
    pub height: f32,
    pub grid_size: f32,
    pub columns: usize,
    pub rows: usize,
    pub center: [f32; 3],
    pub mask: Vec<u8>,
    pub shore_waves: Vec<ShoreWave>,
}
pub(crate) fn u(b: &[u8], p: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        b.get(p..p.checked_add(4).ok_or("overflow")?)
            .ok_or("truncated water data")?
            .try_into()
            .unwrap(),
    ))
}
pub(crate) fn f(b: &[u8], p: usize) -> Result<f32, String> {
    let v = f32::from_bits(u(b, p)?);
    if !v.is_finite() {
        return Err("non-finite water value".into());
    }
    Ok(v)
}
pub(crate) fn path_string(b: &[u8], p: &mut usize) -> Result<String, String> {
    let len = u(b, *p)? as usize;
    *p += 4;
    if len > 1024 {
        return Err("invalid BSData name".into());
    }
    let s = b
        .get(*p..p.checked_add(len).ok_or("overflow")?)
        .ok_or("truncated BSData name")?;
    *p += len;
    let (s, _, bad) = encoding_rs::GBK.decode(s);
    if bad {
        return Err("bad BSData name".into());
    }
    let path = s.replace('\\', "/");
    if path.contains("..") || path.starts_with('/') || path.contains(':') {
        return Err("unsafe BSData path".into());
    }
    Ok(path)
}
pub(crate) fn resolve_case_insensitive(
    root: &Path,
    path: &str,
) -> Result<std::path::PathBuf, String> {
    let mut result = root.to_path_buf();
    for component in path.split('/') {
        let mut found = None;
        for e in fs::read_dir(&result).map_err(|e| e.to_string())? {
            let e = e.map_err(|e| e.to_string())?;
            if e.file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(component)
            {
                found = Some(e.path());
                break;
            }
        }
        result = found.ok_or_else(|| format!("missing BSData path {path}"))?;
    }
    Ok(result)
}
impl WaterArea {
    pub fn contains_xz(&self, x: f32, z: f32) -> bool {
        // 0x7807c6–0x780813 rejects the bounding box boundary before
        // 0x781c50 maps to and clamps the mask cell.
        let half_x = self.columns as f32 * self.grid_size * 0.5;
        let half_z = self.rows as f32 * self.grid_size * 0.5;
        if !x.is_finite()
            || !z.is_finite()
            || (x - self.center[0]).abs() >= half_x
            || (z - self.center[2]).abs() >= half_z
        {
            return false;
        }
        let col = ((x - (self.center[0] - self.columns as f32 * self.grid_size * 0.5))
            / self.grid_size)
            .floor() as isize;
        let row = (((self.center[2] + self.rows as f32 * self.grid_size * 0.5) - z)
            / self.grid_size)
            .floor() as isize;
        if col < 0 || row < 0 || col >= self.columns as isize || row >= self.rows as isize {
            return false;
        }
        self.mask
            .get(row as usize * self.columns + col as usize)
            .is_some_and(|m| *m != 0)
    }
    pub fn parse(b: &[u8], p: usize, export_id: u32) -> Result<Self, String> {
        let version = u(b, p)?;
        if !matches!(version, 0x10002000 | 0x10003000) {
            return Err(format!("unsupported A3DWaterArea version {version:#x}"));
        }
        let color = u(b, p + 8)?;
        // The older v2 area has no simple-water flag byte: its mask begins
        // immediately after the centre vector. v3 inserts the flag at +44.
        let simple = if version == 0x10003000 {
            *b.get(p.checked_add(44).ok_or("offset overflow")?)
                .ok_or("truncated water flag")?
                != 0
        } else {
            false
        };
        let height = f(b, p + 12)?;
        let columns = u(b, p + 20)? as usize;
        let rows = u(b, p + 24)? as usize;
        let grid_size = f(b, p + 28)?;
        let center = [f(b, p + 32)?, f(b, p + 36)?, f(b, p + 40)?];
        if columns == 0
            || rows == 0
            || columns > 4096
            || rows > 4096
            || grid_size <= 0.
            || grid_size > 10000.
        {
            return Err("invalid water grid".into());
        }
        let n = columns.checked_mul(rows).ok_or("grid overflow")?;
        let mask_offset = if version == 0x10003000 { 45 } else { 44 };
        let start = p.checked_add(mask_offset).ok_or("offset overflow")?;
        let mask = b
            .get(start..start.checked_add(n).ok_or("overflow")?)
            .ok_or("truncated water cell mask")?
            .to_vec();
        let wave_start = start.checked_add(n).ok_or("wave offset overflow")?;
        // Invalid shoreline data must not prevent the grid from loading;
        // a clear diagnostic is emitted for this fallback.
        let shore_waves = match parse_shore_waves(b, wave_start) {
            Ok(waves) => waves,
            Err(error) => {
                eprintln!("water {export_id}: shoreline data unavailable: {error}");
                Vec::new()
            }
        };
        Ok(Self {
            export_id,
            color,
            simple,
            height,
            grid_size,
            columns,
            rows,
            center,
            mask,
            shore_waves,
        })
    }
}
fn parse_shore_waves(b: &[u8], offset: usize) -> Result<Vec<ShoreWave>, String> {
    let mut cursor = offset;
    let count = u(b, cursor)? as usize;
    cursor += 4;
    if count > 65536 {
        return Err("invalid shoreline wave count".into());
    }
    let mut shore_waves = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let version = u(b, cursor)?;
        if !matches!(version, 0x10001000 | 0x10002000 | 0x10003000) {
            return Err(format!("unsupported shoreline wave version {version:#x}"));
        }
        let capacity = u(b, cursor + 4)? as usize;
        let width = f(b, cursor + 8)?;
        let points_count = u(b, cursor + 12)? as usize;
        cursor = cursor.checked_add(16).ok_or("wave offset overflow")?;
        // The client's indices are u16 and each point creates three vertices.
        if points_count > capacity || capacity > 21845 || width < 0.0 {
            return Err("invalid shoreline wave dimensions".into());
        }
        let bytes = points_count.checked_mul(12).ok_or("wave size overflow")?;
        let arrays = match version {
            0x10001000 => 1,
            0x10002000 => 2,
            _ => 3,
        };
        let end = cursor
            .checked_add(bytes.checked_mul(arrays).ok_or("wave size overflow")?)
            .ok_or("wave offset overflow")?;
        if end > b.len() {
            return Err("truncated shoreline wave".into());
        }
        let mut points = Vec::with_capacity(points_count);
        for i in 0..points_count {
            let pos = cursor + i * 12;
            points.push([f(b, pos)?, f(b, pos + 4)?, f(b, pos + 8)?]);
        }
        cursor += bytes;
        let mut day_colors = vec![0x80808080; points_count * 3];
        let mut night_colors = vec![0x80808080; points_count * 3];
        if version >= 0x10002000 {
            for (i, color) in day_colors.iter_mut().enumerate() {
                *color = u(b, cursor + i * 4)?;
            }
            cursor += bytes;
            if version == 0x10003000 {
                for (i, color) in night_colors.iter_mut().enumerate() {
                    *color = u(b, cursor + i * 4)?;
                }
                cursor += bytes;
            } else {
                // 0x782863-0x7828b2 converts all ARGB components to floats,
                // scales by 0.3, and packs them back for the night array.
                for (dst, src) in night_colors.iter_mut().zip(&day_colors) {
                    let mut out = 0u32;
                    for shift in [0, 8, 16, 24] {
                        let v = ((src >> shift) & 255) as f32;
                        out |= ((v * 0.3) as u32) << shift;
                    }
                    *dst = out;
                }
            }
        }
        shore_waves.push(ShoreWave {
            capacity,
            width,
            points,
            day_colors,
            night_colors,
        });
    }
    Ok(shore_waves)
}
pub fn load(scene: &Scene, dir: &Path) -> Result<Vec<WaterArea>, String> {
    let b = fs::read(ecbsd_path(dir)).map_err(|e| e.to_string())?;
    if u(&b, 0)? != 0x42584f4d || u(&b, 4)? != 0x42534446 || u(&b, 8)? != 12 {
        return Err("unsupported EC-BSD header".into());
    }
    let count = u(&b, 12)? as usize;
    if count > 10000 {
        return Err("invalid BSData file count".into());
    }
    let mut p = 0x4c;
    let mut files = Vec::with_capacity(count);
    for _ in 0..count {
        files.push(path_string(&b, &mut p)?)
    }
    let mut loaded_index = None;
    let mut loaded = Vec::new();
    let mut records = Vec::new();
    let mut seen = HashMap::<u32, u32>::new();
    let mut areas = Vec::new();
    for block in &scene.blocks {
        for r in &block.water {
            if let Some(previous) = seen.insert(r.export_id, r.offset) {
                if previous != r.offset {
                    return Err(format!("water {} has inconsistent offsets", r.export_id));
                }
                continue;
            }
            records.push((u(&b, r.offset as usize)? as usize, records.len(), r));
        }
    }
    records.sort_by_key(|r| r.0);
    for (index, order, r) in records {
        let off = r.offset as usize;
        let record = u(&b, off + 4)? as usize;
        let path = files
            .get(index)
            .ok_or("water subterrain index out of bounds")?;
        if loaded_index != Some(index) {
            let path = resolve_case_insensitive(dir, path)?;
            drop(std::mem::take(&mut loaded));
            loaded = fs::read(path).map_err(|e| e.to_string())?;
            loaded_index = Some(index);
        }
        let data = &loaded;
        if u(data, 0)? != 0x42584f4d || u(data, 4)? != 2 {
            return Err("unsupported BSData header".into());
        }
        let tail = data.len().checked_sub(4).ok_or("short BSData")?;
        let entries = u(data, tail)? as usize;
        let table = tail
            .checked_sub(entries.checked_mul(4).ok_or("table overflow")?)
            .ok_or("bad BSData table")?;
        if record >= entries {
            return Err("water record index out of bounds".into());
        }
        let rec = u(data, table + record * 4)? as usize;
        let mut end = table;
        for i in 0..entries {
            let other = u(data, table + i * 4)? as usize;
            if other > rec && other < end {
                end = other;
            }
        }
        if rec >= end {
            return Err("invalid water record boundary".into());
        }
        areas.push((order, WaterArea::parse(&data[..end], rec, r.export_id)?));
    }
    areas.sort_by_key(|a| a.0);
    Ok(areas.into_iter().map(|(_, area)| area).collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn underwater_uses_active_cells_and_world_orientation() {
        let a = WaterArea {
            export_id: 1,
            color: 0,
            simple: false,
            height: 5.,
            grid_size: 2.,
            columns: 2,
            rows: 2,
            center: [0., 0., 0.],
            mask: vec![1, 0, 0, 2],
            shore_waves: vec![],
        };
        assert!(a.contains_xz(-1., 1.));
        assert!(!a.contains_xz(1., 1.));
        assert!(a.contains_xz(1., -1.));
        assert!(!a.contains_xz(3., 0.));
        assert!(!a.contains_xz(-2., 1.));
        assert!(!a.contains_xz(-1., 2.));
        assert!(!a.contains_xz(2., -1.));
        assert!(!a.contains_xz(1., -2.));
        assert!(!a.contains_xz(f32::NAN, 0.));
    }
    #[test]
    fn rejects_invalid_version() {
        assert!(WaterArea::parse(&[0; 64], 0, 1).is_err())
    }
    #[test]
    fn decodes_uncompressed_argb_normal_dds() {
        let mut b = vec![0u8; 132];
        b[..4].copy_from_slice(b"DDS ");
        for (p, v) in [
            (4, 124u32),
            (12, 1),
            (16, 1),
            (76, 32),
            (88, 32),
            (92, 0x00ff0000),
            (96, 0x0000ff00),
            (100, 0x000000ff),
            (104, 0xff000000),
        ] {
            b[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[128..132].copy_from_slice(&[120, 254, 131, 255]);
        assert_eq!(
            decode_water_dds(&b).unwrap().get_pixel(0, 0).0,
            [131, 254, 120, 255]
        );
        assert!(decode_water_dds(&b[..131]).is_err());
    }
    #[test]
    fn preserves_authored_water_mips_and_rejects_truncation() {
        let mut b = vec![0; 128 + 4 * 4 * 4 + 2 * 2 * 4 + 4];
        b[..4].copy_from_slice(b"DDS ");
        for (p, v) in [
            (4, 124u32),
            (8, 0x20000),
            (12, 4),
            (16, 4),
            (28, 3),
            (76, 32),
            (88, 32),
            (92, 0x00ff0000),
            (96, 0x0000ff00),
            (100, 0x000000ff),
            (104, 0xff000000),
        ] {
            b[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        let level2 = 128 + 4 * 4 * 4 + 2 * 2 * 4;
        b[level2..level2 + 4].copy_from_slice(&[10, 20, 30, 255]);
        let m = decode_water_dds_mips(&b).unwrap();
        assert_eq!(
            m.iter().map(|x| x.dimensions()).collect::<Vec<_>>(),
            vec![(4, 4), (2, 2), (1, 1)]
        );
        assert_eq!(m[2].get_pixel(0, 0).0, [30, 20, 10, 255]);
        assert!(decode_water_dds_mips(&b[..b.len() - 1]).is_err());
        b[28..32].copy_from_slice(&4u32.to_le_bytes());
        assert!(decode_water_dds_mips(&b).is_err());
    }
    #[test]
    fn shoreline_wave_reads_authored_arrays_and_rejects_truncation() {
        let mut b = vec![0u8; 45 + 1 + 4 + 16 + 36];
        for (p, v) in [
            (0, 0x10003000u32),
            (20, 1),
            (24, 1),
            (28, 32f32.to_bits()),
            (46, 1),
            (50, 0x10003000),
            (54, 1),
            (58, 3f32.to_bits()),
            (62, 1),
            (66, 1f32.to_bits()),
            (70, 2f32.to_bits()),
            (74, 3f32.to_bits()),
            (78, 0xff112233),
            (82, 0xff445566),
            (86, 0xff778899),
            (90, 0xff010203),
            (94, 0xff040506),
            (98, 0xff070809),
        ] {
            b[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        let area = WaterArea::parse(&b, 0, 42).unwrap();
        let wave = &area.shore_waves[0];
        assert_eq!(wave.points, [[1., 2., 3.]]);
        assert_eq!(wave.width, 3.);
        assert_eq!(wave.day_colors, [0xff112233, 0xff445566, 0xff778899]);
        assert_eq!(wave.night_colors, [0xff010203, 0xff040506, 0xff070809]);
        assert!(parse_shore_waves(&b[..b.len() - 1], 46).is_err());
    }
    #[test]
    fn older_shoreline_versions_follow_client_defaults() {
        let mut record = vec![0u8; 16 + 12 + 12];
        for (p, v) in [
            (0, 0x10002000u32),
            (4, 1),
            (8, 2f32.to_bits()),
            (12, 1),
            (16, 1f32.to_bits()),
            (20, 2f32.to_bits()),
            (24, 3f32.to_bits()),
            (28, 0xff112233),
            (32, 0xff445566),
            (36, 0xff778899),
        ] {
            record[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        let mut bytes = 1u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&record);
        let wave = parse_shore_waves(&bytes, 0).unwrap().remove(0);
        assert_eq!(wave.day_colors[0], 0xff112233);
        assert_eq!(wave.night_colors[0], 0x4c050a0f);
        bytes[4..8].copy_from_slice(&0x10001000u32.to_le_bytes());
        bytes.truncate(4 + 16 + 12);
        let wave = parse_shore_waves(&bytes, 0).unwrap().remove(0);
        assert_eq!(wave.day_colors, [0x80808080; 3]);
        assert_eq!(wave.night_colors, [0x80808080; 3]);
    }
    #[test]
    fn actual_69_water_if_available() {
        let Ok(path) = std::env::var("PW_WATER_SAMPLE_69") else {
            return;
        };
        let Ok(b) = std::fs::read(path) else {
            return;
        };
        let area = WaterArea::parse(&b[..0x9df4], 0x3d17, 0).unwrap();
        assert_eq!(area.shore_waves.len(), 8);
        assert_eq!(
            area.shore_waves
                .iter()
                .map(|w| w.points.len())
                .collect::<Vec<_>>(),
            [52, 45, 41, 37, 40, 132, 286, 34]
        );
        assert_eq!(area.shore_waves[0].day_colors[0], 0xff647f94);
        assert_eq!(area.shore_waves[0].night_colors[0], 0xff235175);
    }
    #[test]
    fn parses_legacy_v2_water_without_simple_flag() {
        // Instance v2 records are a 44-byte header followed immediately by the
        // cell mask; there is no simple-water flag at +44.
        let mut b = vec![0; 44 + 4 + 4];
        for (p, v) in [
            (0, 0x10002000u32),
            (8, 0xff123456),
            (12, 216f32.to_bits()),
            (20, 2),
            (24, 2),
            (28, 32f32.to_bits()),
            (32, (-32f32).to_bits()),
            (36, 216f32.to_bits()),
            (40, 32f32.to_bits()),
        ] {
            b[p..p + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[44..48].copy_from_slice(&[1, 0, 0, 1]);
        let area = WaterArea::parse(&b, 0, 19).unwrap();
        assert_eq!(area.mask, [1, 0, 0, 1]);
        assert!(!area.simple);
        assert_eq!(area.columns, 2);
        assert_eq!(area.center, [-32., 216., 32.]);
        assert!(area.shore_waves.is_empty());
    }

    #[test]
    fn parses_simple_water() {
        let mut b = vec![0; 53];
        for (p, v) in [
            (0, 0x10003000u32),
            (8, 0xff123456),
            (12, 10f32.to_bits()),
            (20, 2),
            (24, 2),
            (28, 32f32.to_bits()),
        ] {
            b[p..p + 4].copy_from_slice(&v.to_le_bytes())
        }
        b[45..49].copy_from_slice(&[1, 0, 0, 1]);
        let w = WaterArea::parse(&b, 0, 4).unwrap();
        assert!(!w.simple);
        b[44] = 1;
        assert!(WaterArea::parse(&b, 0, 4).unwrap().simple);
        assert_eq!(w.mask, [1, 0, 0, 1]);
        assert_eq!(w.grid_size, 32.);
    }
}

/// Load the retail client's animated water-normal sequence from its texture package.
pub fn load_normals(element: &Path) -> Result<Vec<Vec<image::RgbaImage>>, String> {
    use std::fs::File;
    let mut file = File::open(element.join("textures.pck")).map_err(|e| e.to_string())?;
    let entries = crate::pck::directory(&mut file)?;
    let mut frames = Vec::new();
    for i in 0..29 {
        let name = format!("textures/water/waternormal/water_normal{i:02}.dds");
        let e = entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(&name))
            .ok_or_else(|| format!("missing {name}"))?;
        let raw = crate::pck::read_entry(&mut file, e)?;
        let img = match decode_water_dds_mips(&raw) {
            Ok(levels) => levels,
            Err(err) => {
                // A malformed or unsupported lower mip must not prevent a
                // client that previously loaded its base level from starting.
                let mut base = raw.clone();
                if base.len() >= 12 {
                    let flags = u32::from_le_bytes(base[8..12].try_into().unwrap()) & !0x20000;
                    base[8..12].copy_from_slice(&flags.to_le_bytes());
                }
                let levels = decode_water_dds_mips(&base).map_err(|e| format!("{name}: {e}"))?;
                eprintln!("{name}: {err}; using base level");
                levels
            }
        };
        if let Some(first) = frames.first() {
            let first: &Vec<image::RgbaImage> = first;
            if img[0].dimensions() != first[0].dimensions() {
                return Err("water normal sequence dimensions differ".into());
            }
        }
        frames.push(img);
    }
    // A texture array needs a common chain length; keep every authored level
    // shared by all frames rather than failing on an unusual archive.
    let levels = frames.iter().map(Vec::len).min().unwrap_or(1);
    for frame in &mut frames {
        frame.truncate(levels);
    }
    Ok(frames)
}

// The retail water-normal DDS is uncompressed 32-bit ARGB, which image's DDS
// decoder does not accept. Preserve all authored mip levels when present.
#[cfg(test)]
fn decode_water_dds(b: &[u8]) -> Result<image::RgbaImage, String> {
    Ok(decode_water_dds_mips(b)?.remove(0))
}
pub(crate) fn decode_water_dds_mips(b: &[u8]) -> Result<Vec<image::RgbaImage>, String> {
    if b.get(..4) != Some(b"DDS ")
        || u(b, 4)? != 124
        || u(b, 76)? != 32
        || u(b, 84)? != 0
        || u(b, 88)? != 32
    {
        return Err("unsupported water DDS layout".into());
    }
    let width = u(b, 16)?;
    let height = u(b, 12)?;
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err("invalid water DDS dimensions".into());
    }
    let masks = [u(b, 92)?, u(b, 96)?, u(b, 100)?, u(b, 104)?];
    if masks[..3].iter().any(|m| *m == 0) {
        return Err("invalid water DDS masks".into());
    }
    let maximum = 32 - width.max(height).leading_zeros();
    let flags = u(b, 8)?;
    let count = if flags & 0x20000 != 0 {
        u(b, 28)?.max(1)
    } else {
        1
    };
    if count > maximum {
        return Err("invalid water DDS mip count".into());
    }
    let mut levels = Vec::with_capacity(count as usize);
    let mut offset = 128usize;
    for level in 0..count {
        let w = (width >> level).max(1);
        let h = (height >> level).max(1);
        let n = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or("water DDS overflow")?;
        let end = offset.checked_add(n).ok_or("water DDS overflow")?;
        let src = b.get(offset..end).ok_or("truncated water DDS mip")?;
        let mut out = Vec::with_capacity(n);
        for pix in src.chunks_exact(4) {
            let v = u32::from_le_bytes(pix.try_into().unwrap());
            for (i, m) in masks.iter().enumerate() {
                if *m == 0 {
                    out.push(if i == 3 { 255 } else { 0 });
                    continue;
                }
                let shift = m.trailing_zeros();
                let max = m >> shift;
                let val = (v & m) >> shift;
                out.push(((val as u64 * 255) / (max as u64)) as u8);
            }
        }
        levels.push(image::RgbaImage::from_raw(w, h, out).ok_or("invalid water DDS pixels")?);
        offset = end;
    }
    Ok(levels)
}

/// Decode the archive's authored DDS mip chain. The image crate decodes only
/// the first level, so each block-compressed level gets a single-level header.
pub(crate) fn decode_texture_mips(raw: &[u8]) -> Result<Vec<image::RgbaImage>, String> {
    let first = match image::load_from_memory_with_format(raw, image::ImageFormat::Dds) {
        Ok(image) => image.to_rgba8(),
        Err(error) => {
            return match decode_water_dds_mips(raw) {
                Ok(levels) => Ok(levels),
                Err(_) => {
                    let mut base = raw.to_vec();
                    if base.len() >= 12 {
                        let flags = u32::from_le_bytes(base[8..12].try_into().unwrap()) & !0x20000;
                        base[8..12].copy_from_slice(&flags.to_le_bytes());
                    }
                    decode_water_dds_mips(&base).map_err(|_| format!("water texture: {error}"))
                }
            };
        }
    };
    let get = |at: usize| {
        raw.get(at..at + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let flags = get(8).unwrap_or(0);
    let count = if flags & 0x20000 != 0 {
        get(28).unwrap_or(1).max(1)
    } else {
        1
    };
    let fourcc = get(84).unwrap_or(0);
    let block = match fourcc {
        0x31545844 => 8usize,
        0x33545844 | 0x35545844 => 16,
        _ => return Ok(vec![first]),
    };
    let mut levels = vec![first];
    let mut offset = 128usize;
    let (mut w, mut h) = levels[0].dimensions();
    let max_levels = 32 - w.max(h).leading_zeros();
    for i in 0..count.min(max_levels) {
        let size = ((w as usize + 3) / 4)
            .checked_mul((h as usize + 3) / 4)
            .and_then(|n| n.checked_mul(block))
            .ok_or("wave mip size overflow")?;
        let end = offset.checked_add(size).ok_or("wave mip offset overflow")?;
        let Some(data) = raw.get(offset..end) else {
            break;
        };
        if i > 0 {
            let mut dds = raw[..128].to_vec();
            dds[8..12].copy_from_slice(&(flags & !0x20000).to_le_bytes());
            // The image DXT decoder requires multiples of four even for
            // legal 1x1 / 2x2 mip blocks; decode padded blocks, then crop.
            let padded_h = (h + 3) & !3;
            let padded_w = (w + 3) & !3;
            dds[12..16].copy_from_slice(&padded_h.to_le_bytes());
            dds[16..20].copy_from_slice(&padded_w.to_le_bytes());
            dds[20..24].copy_from_slice(&(size as u32).to_le_bytes());
            dds[28..32].copy_from_slice(&1u32.to_le_bytes());
            dds.extend_from_slice(data);
            match image::load_from_memory_with_format(&dds, image::ImageFormat::Dds) {
                Ok(img) => {
                    levels.push(image::imageops::crop_imm(&img.to_rgba8(), 0, 0, w, h).to_image())
                }
                Err(_) => break,
            }
        }
        offset = end;
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    Ok(levels)
}

/// Load the client's 30-frame alternate-water texture sequence.
pub fn load_simple_frames(element: &Path) -> Result<Vec<Vec<image::RgbaImage>>, String> {
    use std::fs::File;
    let mut file = File::open(element.join("textures.pck")).map_err(|e| e.to_string())?;
    let entries = crate::pck::directory(&mut file)?;
    let mut frames = Vec::with_capacity(30);
    for i in 0..30 {
        let name = format!("textures/water/l/l{i}.dds");
        let entry = entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(&name))
            .ok_or_else(|| format!("missing {name}"))?;
        let data = crate::pck::read_entry(&mut file, entry)?;
        let levels = decode_texture_mips(&data).map_err(|e| format!("{name}: {e}"))?;
        if let Some(first) = frames.first() {
            let first: &Vec<image::RgbaImage> = first;
            if first[0].dimensions() != levels[0].dimensions() {
                return Err(format!("{name}: dimensions differ"));
            }
        }
        frames.push(levels);
    }
    let count = frames.iter().map(Vec::len).min().unwrap_or(1);
    for frame in &mut frames {
        frame.truncate(count);
    }
    Ok(frames)
}
