//! Persistent disk cache of finished whole-world CPU pages.
//! Derived client data stays below the ignored runtime cache root.
use crate::{
    scene::Ornament,
    world_objects::{Page, PlacementBounds, PlacementRange},
};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const CACHE_MAGIC: &[u8; 8] = b"PWWPAGE1";
const PAGE_MAGIC: &[u8; 8] = b"PWWPG001";
const FORMAT: u64 = 2;
const MAX_CACHED_PAGES: usize = 100_000;
const MAX_CACHED_VERTICES: usize = 16_000_000;
const MAX_CACHED_INDICES: usize = 100_000_000;
const MAX_CACHED_PLACEMENTS: usize = 10_000_000;
const IO_CHUNK: usize = 4 * 1024 * 1024;

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 15) as usize] as char);
    }
    out
}

/// Cheap warm-start identity avoids hashing multi-gigabyte archives at launch.
/// Source archive metadata, ordered world placements, and builder settings
/// participate, so ordinary client patches invalidate the derived pages.
pub(crate) fn identity(
    element: &Path,
    ornaments: &[Ornament],
    order: &[usize],
    vertex_limit: usize,
) -> Option<[u8; 32]> {
    if [
        "PW_WORLD_KEEP_ORDER_TEST",
        "PW_WORLD_TEST_START",
        "PW_WORLD_TEST_LIMIT",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some())
    {
        return None;
    }
    let root = element.canonicalize().ok()?;
    let mut hash = Sha256::new();
    hash.update(b"PWWORLD-FINAL-PAGES-V1");
    hash.update(FORMAT.to_le_bytes());
    hash.update((std::mem::size_of::<crate::gpu::Vertex>() as u64).to_le_bytes());
    hash.update(std::env::consts::ARCH.as_bytes());
    hash.update(root.to_string_lossy().as_bytes());
    hash.update((vertex_limit as u64).to_le_bytes());
    for name in ["litmodels.pck", "building.pck"] {
        let metadata = fs::metadata(root.join(name)).ok()?;
        hash.update(name.as_bytes());
        hash.update(metadata.len().to_le_bytes());
        let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
        hash.update(modified.as_secs().to_le_bytes());
        hash.update(modified.subsec_nanos().to_le_bytes());
    }
    hash.update((order.len() as u64).to_le_bytes());
    for &index in order {
        let item = ornaments.get(index)?;
        hash.update(item.export_id.to_le_bytes());
        hash.update(item.offset.to_le_bytes());
        hash.update(item.extra.to_le_bytes());
        hash.update((item.path.len() as u64).to_le_bytes());
        hash.update(item.path.as_bytes());
        hash.update((item.map_id.len() as u64).to_le_bytes());
        hash.update(item.map_id.as_bytes());
        for value in item
            .position
            .into_iter()
            .chain(item.direction)
            .chain(item.up)
        {
            hash.update(value.to_bits().to_le_bytes());
        }
    }
    Some(hash.finalize().into())
}

fn cache_root() -> PathBuf {
    std::env::var_os("PW_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cache"))
        .join("world-pages-v1")
}
fn final_dir(key: &[u8; 32]) -> PathBuf {
    cache_root().join(hex(key))
}
fn write_hashed(file: &mut File, hash: &mut Sha256, bytes: &[u8]) -> std::io::Result<()> {
    file.write_all(bytes)?;
    hash.update(bytes);
    Ok(())
}
fn write_u64(file: &mut File, hash: &mut Sha256, value: u64) -> std::io::Result<()> {
    write_hashed(file, hash, &value.to_le_bytes())
}
fn write_u32(file: &mut File, hash: &mut Sha256, value: u32) -> std::io::Result<()> {
    write_hashed(file, hash, &value.to_le_bytes())
}
fn write_f32(file: &mut File, hash: &mut Sha256, value: f32) -> std::io::Result<()> {
    write_u32(file, hash, value.to_bits())
}

pub(crate) struct Writer {
    key: [u8; 32],
    temp: PathBuf,
    final_dir: PathBuf,
    pages_file: Option<File>,
    pages: usize,
    published: bool,
}
impl Writer {
    pub(crate) fn new(key: [u8; 32]) -> std::io::Result<Self> {
        let parent = cache_root();
        fs::create_dir_all(&parent)?;
        let final_dir = final_dir(&key);
        let nonce = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp = parent.join(format!(
            "{}.{}.{}.tmp",
            hex(&key),
            std::process::id(),
            nonce
        ));
        fs::create_dir(&temp)?;
        let pages_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temp.join("pages.bin"))?;
        Ok(Self {
            key,
            temp,
            final_dir,
            pages_file: Some(pages_file),
            pages: 0,
            published: false,
        })
    }
    pub(crate) fn write_page(&mut self, page: &Page) -> std::io::Result<()> {
        let file = self.pages_file.as_mut().expect("page cache file open");
        let mut hash = Sha256::new();
        write_hashed(file, &mut hash, PAGE_MAGIC)?;
        for value in [
            page.processed as u64,
            page.used_height as u64,
            page.shared_atlas.map(|n| n as u64).unwrap_or(u64::MAX),
            page.vertices.len() as u64,
            page.indices.len() as u64,
            page.atlas.len() as u64,
            page.mips.len() as u64,
            page.placements.len() as u64,
            page.bounds.len() as u64,
        ] {
            write_u64(file, &mut hash, value)?;
        }
        write_hashed(file, &mut hash, &[u8::from(page.page_has_sdr)])?;
        for value in page.page_min.into_iter().chain(page.page_max) {
            write_f32(file, &mut hash, value)?;
        }
        write_hashed(file, &mut hash, bytemuck::cast_slice(&page.vertices))?;
        write_hashed(file, &mut hash, bytemuck::cast_slice(&page.indices))?;
        write_hashed(file, &mut hash, &page.atlas)?;
        for mip in &page.mips {
            write_u64(file, &mut hash, mip.len() as u64)?;
            write_hashed(file, &mut hash, mip)?;
        }
        for placement in &page.placements {
            write_u32(file, &mut hash, placement.start)?;
            write_u32(file, &mut hash, placement.end)?;
            write_u32(file, &mut hash, placement.export_id)?;
            write_u64(file, &mut hash, placement.order)?;
            write_hashed(
                file,
                &mut hash,
                &[
                    u8::from(placement.visibility.reflect),
                    u8::from(placement.visibility.refract),
                ],
            )?;
        }
        for bound in &page.bounds {
            for value in bound.min.into_iter().chain(bound.max) {
                write_f32(file, &mut hash, value)?;
            }
            write_hashed(file, &mut hash, &[u8::from(bound.has_sdr)])?;
        }
        file.write_all(&hash.finalize())?;
        self.pages += 1;
        Ok(())
    }
    pub(crate) fn finish(
        mut self,
        processed: usize,
        skipped: usize,
        atlas_pages: usize,
        atlas_bytes: usize,
    ) -> std::io::Result<()> {
        let mut pages_file = self.pages_file.take().expect("page cache file open");
        pages_file.flush()?;
        pages_file.sync_all()?;
        drop(pages_file);
        let manifest_tmp = self.temp.join("manifest.tmp");
        let mut file = File::create(&manifest_tmp)?;
        let mut bytes = Vec::with_capacity(88);
        bytes.extend_from_slice(CACHE_MAGIC);
        bytes.extend_from_slice(&self.key);
        bytes.extend_from_slice(&FORMAT.to_le_bytes());
        bytes.extend_from_slice(&(self.pages as u64).to_le_bytes());
        bytes.extend_from_slice(&(processed as u64).to_le_bytes());
        bytes.extend_from_slice(&(skipped as u64).to_le_bytes());
        bytes.extend_from_slice(&(atlas_pages as u64).to_le_bytes());
        bytes.extend_from_slice(&(atlas_bytes as u64).to_le_bytes());
        let digest = Sha256::digest(&bytes);
        file.write_all(&bytes)?;
        file.write_all(&digest)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&manifest_tmp, self.temp.join("manifest.bin"))?;
        if self.final_dir.exists() {
            fs::remove_dir_all(&self.final_dir)?;
        }
        fs::rename(&self.temp, &self.final_dir)?;
        self.published = true;
        Ok(())
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.temp);
        }
    }
}

pub(crate) struct Reader {
    dir: PathBuf,
    pages_file: Option<File>,
    pages: usize,
    next_page: usize,
    pub(crate) processed: usize,
    pub(crate) skipped: usize,
    pub(crate) atlas_pages: usize,
    pub(crate) atlas_bytes: usize,
}
impl Reader {
    pub(crate) fn open(key: &[u8; 32]) -> Option<Self> {
        let dir = final_dir(key);
        let bytes = fs::read(dir.join("manifest.bin")).ok()?;
        if bytes.len() != 8 + 32 + 8 * 6 + 32 || &bytes[..8] != CACHE_MAGIC {
            return None;
        }
        let split = bytes.len() - 32;
        if Sha256::digest(&bytes[..split]).as_slice() != &bytes[split..] {
            return None;
        }
        if &bytes[8..40] != key || u64::from_le_bytes(bytes[40..48].try_into().ok()?) != FORMAT {
            return None;
        }
        let pages = usize::try_from(u64::from_le_bytes(bytes[48..56].try_into().ok()?)).ok()?;
        if pages > MAX_CACHED_PAGES {
            return None;
        }
        let processed = usize::try_from(u64::from_le_bytes(bytes[56..64].try_into().ok()?)).ok()?;
        let skipped = usize::try_from(u64::from_le_bytes(bytes[64..72].try_into().ok()?)).ok()?;
        let atlas_pages =
            usize::try_from(u64::from_le_bytes(bytes[72..80].try_into().ok()?)).ok()?;
        let atlas_bytes =
            usize::try_from(u64::from_le_bytes(bytes[80..88].try_into().ok()?)).ok()?;
        if atlas_pages > pages {
            return None;
        }
        let pages_file = File::open(dir.join("pages.bin")).ok()?;
        Some(Self {
            dir,
            pages_file: Some(pages_file),
            pages,
            next_page: 0,
            processed,
            skipped,
            atlas_pages,
            atlas_bytes,
        })
    }
    pub(crate) fn page_count(&self) -> usize {
        self.pages
    }
    pub(crate) fn invalidate(&mut self) {
        self.pages_file = None;
        let _ = fs::remove_dir_all(&self.dir);
    }
    pub(crate) fn complete(&mut self) -> Result<(), String> {
        if self.next_page != self.pages {
            return Err("incomplete page-cache traversal".into());
        }
        let mut trailing = [0; 1];
        if self
            .pages_file
            .as_mut()
            .ok_or("page cache file closed")?
            .read(&mut trailing)
            .map_err(|e| e.to_string())?
            != 0
        {
            return Err("trailing bytes in cached world page stream".into());
        }
        Ok(())
    }
    pub(crate) fn read_page(&mut self, index: usize) -> Result<Page, String> {
        if index != self.next_page || index >= self.pages {
            return Err("page-cache read order mismatch".into());
        }
        let file = self.pages_file.as_mut().ok_or("page cache file closed")?;
        let mut hash = Sha256::new();
        let mut magic = [0; 8];
        read_hashed(file, &mut hash, &mut magic)?;
        if &magic != PAGE_MAGIC {
            return Err("bad page-cache record magic".into());
        }
        let processed =
            usize::try_from(read_u64(file, &mut hash)?).map_err(|_| "processed count overflow")?;
        let used_height =
            usize::try_from(read_u64(file, &mut hash)?).map_err(|_| "page height overflow")?;
        let shared = read_u64(file, &mut hash)?;
        let vertex_count = checked_count(read_u64(file, &mut hash)?, MAX_CACHED_VERTICES)?;
        let index_count = checked_count(read_u64(file, &mut hash)?, MAX_CACHED_INDICES)?;
        let atlas_len = checked_count(
            read_u64(file, &mut hash)?,
            crate::world_objects::SIDE * crate::world_objects::SIDE * 4,
        )?;
        let mip_count = checked_count(read_u64(file, &mut hash)?, 7)?;
        let placement_count = checked_count(read_u64(file, &mut hash)?, MAX_CACHED_PLACEMENTS)?;
        let bounds_count = checked_count(read_u64(file, &mut hash)?, MAX_CACHED_PLACEMENTS)?;
        if !(128..=crate::world_objects::SIDE).contains(&used_height)
            || mip_count != if shared == u64::MAX { 7 } else { 0 }
            || bounds_count != placement_count
        {
            return Err("invalid cached page dimensions".into());
        }
        let mut byte = [0; 1];
        read_hashed(file, &mut hash, &mut byte)?;
        let page_has_sdr = byte[0] != 0;
        let mut page_min = [0.; 3];
        let mut page_max = [0.; 3];
        for value in page_min.iter_mut().chain(page_max.iter_mut()) {
            *value = read_f32(file, &mut hash)?;
        }
        let vertices = read_pod_vec::<crate::gpu::Vertex>(file, &mut hash, vertex_count)?;
        let indices = read_pod_vec::<u32>(file, &mut hash, index_count)?;
        if indices.iter().any(|&index| index as usize >= vertex_count) {
            return Err("cached page contains an out-of-range vertex index".into());
        }
        let atlas = read_byte_vec(file, &mut hash, atlas_len)?;
        let mut mips = Vec::with_capacity(mip_count);
        for level in 1..=mip_count {
            let expected = (crate::world_objects::SIDE >> level) * (used_height >> level) * 4;
            let len = checked_count(read_u64(file, &mut hash)?, expected)?;
            if len != expected {
                return Err("invalid cached mip size".into());
            }
            mips.push(read_byte_vec(file, &mut hash, len)?);
        }
        let mut placements = Vec::with_capacity(placement_count);
        for _ in 0..placement_count {
            let start = read_u32(file, &mut hash)?;
            let end = read_u32(file, &mut hash)?;
            let export_id = read_u32(file, &mut hash)?;
            let order = read_u64(file, &mut hash)?;
            let mut flags = [0; 2];
            read_hashed(file, &mut hash, &mut flags)?;
            if start > end || end as usize > index_count {
                return Err("invalid cached placement index range".into());
            }
            placements.push(PlacementRange {
                start,
                end,
                export_id,
                order,
                visibility: crate::scenery_visibility::Visibility {
                    reflect: flags[0] != 0,
                    refract: flags[1] != 0,
                },
            });
        }
        let mut bounds = Vec::with_capacity(bounds_count);
        for _ in 0..bounds_count {
            let mut min = [0.; 3];
            let mut max = [0.; 3];
            for value in min.iter_mut().chain(max.iter_mut()) {
                *value = read_f32(file, &mut hash)?;
            }
            read_hashed(file, &mut hash, &mut byte)?;
            bounds.push(PlacementBounds {
                min,
                max,
                has_sdr: byte[0] != 0,
            });
        }
        let mut expected_hash = [0; 32];
        file.read_exact(&mut expected_hash)
            .map_err(|e| e.to_string())?;
        let actual_hash = hash.finalize();
        if expected_hash.as_slice() != actual_hash.as_slice() {
            return Err("cached world page checksum mismatch".into());
        }
        if shared == u64::MAX && atlas.len() != crate::world_objects::SIDE * used_height * 4 {
            return Err("cached atlas size mismatch".into());
        }
        if shared != u64::MAX && atlas_len != 0 {
            return Err("shared page contains an atlas copy".into());
        }
        let shared_atlas = if shared == u64::MAX {
            None
        } else {
            Some(usize::try_from(shared).map_err(|_| "shared atlas index overflow")?)
        };
        let page = Page {
            vertices,
            indices,
            atlas,
            shared_atlas,
            processed,
            used_height,
            mips,
            placements,
            bounds,
            page_min,
            page_max,
            page_has_sdr,
        };
        self.next_page += 1;
        Ok(page)
    }
}

fn checked_count(value: u64, max: usize) -> Result<usize, String> {
    let value = usize::try_from(value).map_err(|_| "page-cache length overflow")?;
    if value > max {
        return Err("page-cache allocation limit exceeded".into());
    }
    Ok(value)
}
fn read_hashed(file: &mut File, hash: &mut Sha256, bytes: &mut [u8]) -> Result<(), String> {
    file.read_exact(bytes).map_err(|e| e.to_string())?;
    hash.update(bytes);
    Ok(())
}
fn read_u64(file: &mut File, hash: &mut Sha256) -> Result<u64, String> {
    let mut b = [0; 8];
    read_hashed(file, hash, &mut b)?;
    Ok(u64::from_le_bytes(b))
}
fn read_u32(file: &mut File, hash: &mut Sha256) -> Result<u32, String> {
    let mut b = [0; 4];
    read_hashed(file, hash, &mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn read_f32(file: &mut File, hash: &mut Sha256) -> Result<f32, String> {
    Ok(f32::from_bits(read_u32(file, hash)?))
}
fn read_byte_vec(file: &mut File, hash: &mut Sha256, len: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(len);
    let mut scratch = vec![0; IO_CHUNK.min(len.max(1))];
    let mut left = len;
    while left != 0 {
        let n = left.min(scratch.len());
        read_hashed(file, hash, &mut scratch[..n])?;
        out.extend_from_slice(&scratch[..n]);
        left -= n;
    }
    Ok(out)
}
fn read_pod_vec<T: bytemuck::Pod + bytemuck::Zeroable + Copy>(
    file: &mut File,
    hash: &mut Sha256,
    count: usize,
) -> Result<Vec<T>, String> {
    let total = count
        .checked_mul(std::mem::size_of::<T>())
        .ok_or("page-cache vector size overflow")?;
    let zero = <T as bytemuck::Zeroable>::zeroed();
    let mut out = vec![zero; count];
    let bytes = bytemuck::cast_slice_mut(&mut out);
    let size = std::mem::size_of::<T>();
    let chunk = (IO_CHUNK / size).max(1) * size;
    let mut offset = 0;
    while offset < total {
        let n = (total - offset).min(chunk);
        read_hashed(file, hash, &mut bytes[offset..offset + n])?;
        offset += n;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::Vertex;
    #[test]
    fn page_cache_round_trips_page_data_and_checksums() {
        let nonce = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let digest = Sha256::digest(nonce.to_le_bytes());
        let mut key = [0; 32];
        key.copy_from_slice(&digest);
        let page = Page {
            vertices: vec![Vertex {
                pos: [1., 2., 3.],
                normal: [0., 1., 0.],
                uv: [0.1, 0.2],
                color: [0.; 4],
                kind: 2.,
                diffuse: 7,
                night_diffuse: 9,
            }],
            indices: vec![0],
            atlas: vec![0; crate::world_objects::SIDE * 128 * 4],
            shared_atlas: None,
            processed: 5,
            used_height: 128,
            mips: Vec::new(),
            placements: vec![PlacementRange {
                start: 0,
                end: 1,
                export_id: 4,
                order: 12,
                visibility: crate::scenery_visibility::Visibility {
                    reflect: true,
                    refract: false,
                },
            }],
            bounds: Vec::new(),
            page_min: [f32::INFINITY; 3],
            page_max: [f32::NEG_INFINITY; 3],
            page_has_sdr: false,
        }
        .finish();
        let mut writer = Writer::new(key).unwrap();
        writer.write_page(&page).unwrap();
        writer.finish(5, 0, 0, 0).unwrap();
        let mut reader = Reader::open(&key).unwrap();
        let restored = reader.read_page(0).unwrap();
        reader.complete().unwrap();
        assert_eq!(restored.vertices[0].pos, page.vertices[0].pos);
        assert_eq!(restored.placements[0].order, 12);
        assert_eq!(restored.placements[0].visibility.reflect, true);
        reader.invalidate();
    }
}
