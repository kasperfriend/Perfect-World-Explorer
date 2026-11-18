//! Lossless terrain paging: XOR byte planes on disk, a bounded decoded hot set
//! in RAM. Source floats are never quantized. Navigation and mesh workers share
//! one cache, and cursors pin just their current block outside the hot set.
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
/// Decoded hot-set budget in bytes when `PW_TERRAIN_RAM_MIB` is unset.
///
/// The largest recovered block window (`radius` 12) is 25 x 25 blocks of
/// `(grid + 1)^2` cells, i.e. about 10 MiB of decoded pages, so 32 MiB holds a
/// whole window plus its neighbours and the in-viewer rebuild stops decoding
/// the same blocks again. Smaller budgets trade RAM for I/O stalls; the
/// environment override still accepts 1..256 MiB.
const DEFAULT_BUDGET: usize = 16 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(0);
#[derive(Clone)]
struct Record {
    offset: u64,
    size: usize,
    hash: [u8; 32],
}
struct Page {
    heights: Vec<f32>,
    normals: Vec<[i8; 3]>,
}
impl Page {
    fn bytes(&self) -> usize {
        self.heights.capacity() * 4
            + self.normals.capacity() * 3
            + std::mem::size_of::<Page>()
            + 2 * std::mem::size_of::<usize>()
    }
}
/// The whole decoded grid, kept instead of paged. This is the resident mode the
/// optimization work replaced: no disk object, no zlib block decode and no
/// per-block SHA-256 at read time.
struct Resident {
    heights: Vec<f32>,
    normals: Vec<[i8; 3]>,
}
impl Resident {
    fn bytes(&self) -> usize {
        self.heights.capacity() * 4 + self.normals.capacity() * 3 + std::mem::size_of::<Resident>()
    }
}
struct Entry {
    page: Arc<Page>,
    used: u64,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub resident_bytes: usize,
    pub peak_bytes: usize,
    pub budget_bytes: usize,
    pub reads: usize,
    pub evictions: usize,
    pub metadata_bytes: usize,
}
struct Hot {
    pages: HashMap<usize, Entry>,
    clock: u64,
    stats: Stats,
}
struct Disk {
    file: File,
    path: PathBuf,
    side: usize,
    records: Vec<Record>,
    hot: Hot,
}
enum Data {
    /// Heights only; used by small fixtures and diagnostics.
    Memory(Vec<f32>),
    /// Heights and normals fully decoded and resident.
    Resident(Resident),
    Disk(Mutex<Disk>),
}
struct Store {
    len: usize,
    width: usize,
    side: usize,
    data: Data,
}
#[derive(Clone)]
pub struct Heights(Arc<Store>);
pub struct Normals(Arc<Store>);
pub struct Reader {
    store: Arc<Store>,
    current: Option<(usize, Arc<Page>)>,
}
impl From<Vec<f32>> for Heights {
    fn from(values: Vec<f32>) -> Self {
        Self(Arc::new(Store {
            len: values.len(),
            width: values.len(),
            side: 1,
            data: Data::Memory(values),
        }))
    }
}
impl Heights {
    pub fn reader(&self) -> Reader {
        Reader {
            store: Arc::clone(&self.0),
            current: None,
        }
    }
    pub fn at(&self, index: usize) -> f32 {
        match &self.0.data {
            Data::Memory(v) => v[index],
            Data::Resident(grid) => grid.heights[index],
            Data::Disk(_) => self.reader().height(index),
        }
    }
    pub fn stats(&self) -> Stats {
        match &self.0.data {
            Data::Memory(v) => Stats {
                resident_bytes: v.len() * 4,
                ..Stats::default()
            },
            Data::Resident(grid) => Stats {
                resident_bytes: grid.bytes(),
                budget_bytes: grid.bytes(),
                ..Stats::default()
            },
            Data::Disk(d) => {
                let d = d.lock().unwrap();
                Stats {
                    metadata_bytes: d.records.len() * std::mem::size_of::<Record>(),
                    ..d.hot.stats
                }
            }
        }
    }
    /// Release even the hot set after an exhaustive initialization/export scan.
    /// The resident grid is the point of that mode, so it is never released.
    pub fn trim(&self) {
        if let Data::Disk(d) = &self.0.data {
            let mut d = d.lock().unwrap();
            d.hot.pages.clear();
            d.hot.stats.resident_bytes = 0;
        }
    }
    /// Whether this store holds the whole grid instead of paging it.
    pub fn is_resident(&self) -> bool {
        matches!(self.0.data, Data::Resident(_) | Data::Memory(_))
    }
}
impl Normals {
    pub fn at(&self, index: usize) -> [i8; 3] {
        if let Data::Resident(grid) = &self.0.data {
            return grid.normals[index];
        }
        Reader {
            store: Arc::clone(&self.0),
            current: None,
        }
        .normal(index)
    }
}
impl Reader {
    fn page(&mut self, index: usize) -> Result<usize, String> {
        if index >= self.store.len {
            return Err(format!("terrain cell {index} out of bounds"));
        }
        let width = self.store.width;
        let side = self.store.side;
        let x = index % width;
        let y = index / width;
        let id = (y / side) * (width / side) + x / side;
        let local = (y % side) * side + x % side;
        if let Some((last, _)) = &self.current {
            if *last == id {
                return Ok(local);
            }
        }
        let Data::Disk(disk) = &self.store.data else {
            return Err("memory fixture has no disk pages".into());
        };
        let mut d = disk
            .lock()
            .map_err(|_| "terrain page cache lock poisoned")?;
        d.hot.clock += 1;
        let clock = d.hot.clock;
        if let Some(entry) = d.hot.pages.get_mut(&id) {
            entry.used = clock;
            let page = Arc::clone(&entry.page);
            self.current = Some((id, page));
            return Ok(local);
        }
        let record = d.records[id].clone();
        let mut packed = vec![0; record.size];
        d.file
            .seek(SeekFrom::Start(record.offset))
            .and_then(|_| d.file.read_exact(&mut packed))
            .map_err(|e| format!("terrain cache {}: {e}", d.path.display()))?;
        let cells = d.side * d.side;
        let mut bytes = Vec::with_capacity(cells * 7);
        ZlibDecoder::new(&packed[..])
            .take((cells * 7 + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("terrain cache block {id}: {e}; restart to rebuild cache"))?;
        if bytes.len() != cells * 7 || <[u8; 32]>::from(Sha256::digest(&bytes)) != record.hash {
            return Err(format!(
                "corrupt terrain cache block {id}; restart to rebuild cache"
            ));
        }
        let page = Arc::new(decode(&bytes, d.side));
        let size = page.bytes();
        while d.hot.stats.resident_bytes + size > d.hot.stats.budget_bytes
            && !d.hot.pages.is_empty()
        {
            let victim = *d.hot.pages.iter().min_by_key(|(_, e)| e.used).unwrap().0;
            let removed = d.hot.pages.remove(&victim).unwrap();
            d.hot.stats.resident_bytes -= removed.page.bytes();
            d.hot.stats.evictions += 1;
        }
        if size <= d.hot.stats.budget_bytes {
            d.hot.pages.insert(
                id,
                Entry {
                    page: Arc::clone(&page),
                    used: clock,
                },
            );
            d.hot.stats.resident_bytes += size;
            d.hot.stats.peak_bytes = d.hot.stats.peak_bytes.max(d.hot.stats.resident_bytes);
        }
        d.hot.stats.reads += 1;
        self.current = Some((id, page));
        Ok(local)
    }
    pub fn cell(&mut self, index: usize) -> Result<(f32, [i8; 3]), String> {
        match &self.store.data {
            Data::Memory(values) => {
                return values
                    .get(index)
                    .copied()
                    .map(|h| (h, [0, 127, 0]))
                    .ok_or_else(|| format!("terrain cell {index} out of bounds"));
            }
            Data::Resident(grid) => {
                return grid
                    .heights
                    .get(index)
                    .zip(grid.normals.get(index))
                    .map(|(h, n)| (*h, *n))
                    .ok_or_else(|| format!("terrain cell {index} out of bounds"));
            }
            Data::Disk(_) => {}
        }
        let local = self.page(index)?;
        let page = &self.current.as_ref().unwrap().1;
        Ok((page.heights[local], page.normals[local]))
    }
    pub fn height(&mut self, index: usize) -> f32 {
        match &self.store.data {
            Data::Memory(values) => return values[index],
            Data::Resident(grid) => return grid.heights[index],
            Data::Disk(_) => {}
        }
        let local = self
            .page(index)
            .expect("lossless terrain backing store read failed");
        self.current.as_ref().unwrap().1.heights[local]
    }
    pub fn normal(&mut self, index: usize) -> [i8; 3] {
        match &self.store.data {
            Data::Memory(_) => {
                assert!(index < self.store.len);
                return [0, 127, 0];
            }
            Data::Resident(grid) => return grid.normals[index],
            Data::Disk(_) => {}
        }
        let local = self
            .page(index)
            .expect("lossless terrain backing store read failed");
        self.current.as_ref().unwrap().1.normals[local]
    }
}
// Deinterleaving plus row-local XOR exposes equal float exponent bytes and
// correlated normals to the compressor without discarding a single source bit.
fn encode(heights: &[f32], normals: &[[i8; 3]], side: usize) -> Vec<u8> {
    let cells = side * side;
    let mut out = vec![0; cells * 7];
    for plane in 0..7 {
        for y in 0..side {
            let mut previous = 0;
            for x in 0..side {
                let i = y * side + x;
                let value = if plane < 4 {
                    heights[i].to_bits().to_le_bytes()[plane]
                } else {
                    normals[i][plane - 4] as u8
                };
                out[plane * cells + i] = value ^ previous;
                previous = value;
            }
        }
    }
    out
}
fn decode(bytes: &[u8], side: usize) -> Page {
    let cells = side * side;
    let mut raw = bytes.to_vec();
    for plane in 0..7 {
        for y in 0..side {
            let mut previous = 0;
            for x in 0..side {
                let i = plane * cells + y * side + x;
                raw[i] ^= previous;
                previous = raw[i];
            }
        }
    }
    let mut heights = Vec::with_capacity(cells);
    let mut normals = Vec::with_capacity(cells);
    for i in 0..cells {
        heights.push(f32::from_bits(u32::from_le_bytes([
            raw[i],
            raw[cells + i],
            raw[2 * cells + i],
            raw[3 * cells + i],
        ])));
        normals.push([
            raw[4 * cells + i] as i8,
            raw[5 * cells + i] as i8,
            raw[6 * cells + i] as i8,
        ]);
    }
    Page { heights, normals }
}
pub struct Builder {
    file: Option<File>,
    path: PathBuf,
    root: PathBuf,
    width: usize,
    height: usize,
    side: usize,
    records: Vec<Option<Record>>,
    hash: Sha256,
    /// `Some` in resident mode: the decoded grid replaces the disk object.
    resident: Option<Resident>,
    /// Which resident pages have been written, for duplicate/missing checks.
    filled: Vec<bool>,
    /// Source-content identity of the client files this grid came from.
    identity: Option<String>,
}
impl Builder {
    fn validate(width: usize, height: usize, side: usize) -> Result<(), String> {
        if side == 0
            || side > 256
            || width == 0
            || height == 0
            || width % side != 0
            || height % side != 0
            || width
                .checked_mul(height)
                .is_none_or(|cells| cells > 100_000_000)
            || (width / side)
                .checked_mul(height / side)
                .is_none_or(|pages| pages > 1_000_000)
        {
            return Err("invalid paged terrain dimensions".into());
        }
        Ok(())
    }
    pub fn new(root: &Path, width: usize, height: usize, side: usize) -> Result<Self, String> {
        Self::validate(width, height, side)?;
        fs::create_dir_all(root).map_err(|e| format!("terrain cache {}: {e}", root.display()))?;
        let path = root.join(format!(
            "session-{}-{}-{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let mut header = b"PWTGRID1".to_vec();
        for v in [width, height, side] {
            header.extend_from_slice(&(v as u64).to_le_bytes());
        }
        file.write_all(&header).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        hash.update(&header);
        Ok(Self {
            file: Some(file),
            path,
            root: root.to_path_buf(),
            width,
            height,
            side,
            records: vec![None; (width / side) * (height / side)],
            hash,
            resident: None,
            filled: Vec::new(),
            identity: None,
        })
    }
    /// Resident mode: keep the whole decoded grid in memory and never touch the
    /// disk. This is the fast path the paged cache replaced; it exists as an
    /// explicit option because it costs the full 7 bytes per cell
    /// (`width * height * 7` — 154 MiB for a 4096 x 5632 world).
    pub fn new_resident(width: usize, height: usize, side: usize) -> Result<Self, String> {
        Self::validate(width, height, side)?;
        let cells = width * height;
        let pages = (width / side) * (height / side);
        Ok(Self {
            file: None,
            path: PathBuf::new(),
            root: PathBuf::new(),
            width,
            height,
            side,
            records: vec![None; pages],
            hash: Sha256::new(),
            resident: Some(Resident {
                heights: vec![0.; cells],
                normals: vec![[0; 3]; cells],
            }),
            filled: vec![false; pages],
            identity: None,
        })
    }
    /// Attach the source-content identity that names this grid's manifest, so a
    /// warm start can skip the block decode and recompression entirely. Only the
    /// paged mode writes a manifest; the resident mode has nothing to cache.
    pub fn with_identity(mut self, identity: &str) -> Self {
        self.identity = Some(identity.to_string());
        self
    }
    /// Drop everything and remove a half-written session object.
    fn abandon(&mut self) {
        self.resident = None;
        if self.file.take().is_some() && !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
    pub fn block(
        &mut self,
        bx: usize,
        by: usize,
        heights: &[f32],
        normals: &[[i8; 3]],
    ) -> Result<(), String> {
        if bx >= self.width / self.side
            || by >= self.height / self.side
            || heights.len() != self.side * self.side
            || normals.len() != heights.len()
        {
            return Err("invalid terrain cache block".into());
        }
        let id = by * (self.width / self.side) + bx;
        if self.resident.is_some() {
            if self.filled[id] {
                return Err("duplicate terrain cache block".into());
            }
            self.filled[id] = true;
            let (side, width) = (self.side, self.width);
            let grid = self.resident.as_mut().unwrap();
            for y in 0..side {
                for x in 0..side {
                    let source = y * side + x;
                    let target = (by * side + y) * width + bx * side + x;
                    grid.heights[target] = heights[source];
                    grid.normals[target] = normals[source];
                }
            }
            return Ok(());
        }
        if self.records[id].is_some() {
            return Err("duplicate terrain cache block".into());
        }
        let raw = encode(heights, normals, self.side);
        let hash = Sha256::digest(&raw).into();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&raw).map_err(|e| e.to_string())?;
        let packed = encoder.finish().map_err(|e| e.to_string())?;
        let file = self.file.as_mut().unwrap();
        let mut header = (id as u64).to_le_bytes().to_vec();
        header.extend_from_slice(&(packed.len() as u64).to_le_bytes());
        file.write_all(&header)
            .and_then(|_| file.write_all(&packed))
            .map_err(|e| format!("terrain cache write: {e}"))?;
        self.hash.update(&header);
        self.hash.update(&packed);
        let offset = file.stream_position().map_err(|e| e.to_string())? - packed.len() as u64;
        self.records[id] = Some(Record {
            offset,
            size: packed.len(),
            hash,
        });
        Ok(())
    }
    pub fn finish(mut self, budget: usize) -> Result<(Heights, Normals), String> {
        if let Some(grid) = self.resident.take() {
            if self.filled.iter().any(|written| !*written) {
                self.abandon();
                return Err("incomplete terrain resident grid".into());
            }
            let len = self.width * self.height;
            eprintln!(
                "Resident terrain (All to VRAM): {} cells, {}+{} MiB decoded grid held in memory; no disk cache, no block decode",
                len,
                self.width * self.height * 4 >> 20,
                self.width * self.height * 3 >> 20
            );
            let store = Arc::new(Store {
                len,
                width: self.width,
                side: self.side,
                data: Data::Resident(grid),
            });
            return Ok((Heights(Arc::clone(&store)), Normals(store)));
        }
        if self.records.iter().any(Option::is_none) {
            self.abandon();
            return Err("incomplete terrain page cache".into());
        }
        let file = self.file.take().unwrap();
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        let digest = self.hash.clone().finalize();
        let name = format!("{:x}.pages", digest);
        let target = self.root.join(name);
        // Never trust existence, timestamps or client filenames as identity.
        // Stream-verify old content before reusing an immutable cache object.
        let valid = File::open(&target).ok().is_some_and(|mut f| {
            let mut hash = Sha256::new();
            let mut buf = [0u8; 65536];
            loop {
                match f.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => hash.update(&buf[..n]),
                    Err(_) => return false,
                }
            }
            hash.finalize() == digest
        });
        if valid {
            fs::remove_file(&self.path).map_err(|e| e.to_string())?;
            self.path = target;
        } else if fs::rename(&self.path, &target).is_ok() {
            self.path = target;
        }
        // On Windows a corrupt old file may be held open by another process.
        // In that case keep our verified new session file, never overwrite it.
        let file = File::open(&self.path).map_err(|e| e.to_string())?;
        let records: Vec<Record> = std::mem::take(&mut self.records)
            .into_iter()
            .map(Option::unwrap)
            .collect();
        let disk_bytes = file.metadata().map_err(|e| e.to_string())?.len();
        eprintln!("Lossless terrain paging: {} cells, {:.2} MiB source grids -> {:.2} MiB disk; decoded hot-set budget {:.2} MiB (plus one block per reader)",
            self.width*self.height,(self.width*self.height*7) as f64/1048576.,disk_bytes as f64/1048576.,budget as f64/1048576.);
        // Publish the index that lets the next start skip the block decode and
        // the whole recompression pass. A failure here only costs that warm
        // start; this session's verified pages file stays usable and correct.
        if let Some(identity) = self.identity.clone() {
            let pages_name = self
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            match write_manifest(
                &self.root,
                &identity,
                self.width,
                self.height,
                self.side,
                &pages_name,
                digest.as_slice(),
                &records,
            ) {
                Ok(()) => eprintln!(
                    "Terrain cache index: {} (warm starts reuse it instead of rebuilding)",
                    identity
                ),
                Err(e) => eprintln!(
                    "Terrain cache index unavailable ({e}); the next start rebuilds the pages from the client"
                ),
            }
        }
        let store = Arc::new(Store {
            len: self.width * self.height,
            width: self.width,
            side: self.side,
            data: Data::Disk(Mutex::new(Disk {
                file,
                path: self.path.clone(),
                side: self.side,
                records,
                hot: Hot {
                    pages: HashMap::new(),
                    clock: 0,
                    stats: Stats {
                        budget_bytes: budget,
                        ..Stats::default()
                    },
                },
            })),
        });
        Ok((Heights(Arc::clone(&store)), Normals(store)))
    }
}
impl Drop for Builder {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _ = fs::remove_file(&self.path);
        }
    }
}
/// Magic of the warm-start index that sits beside a pages object.
const MANIFEST_MAGIC: &[u8; 8] = b"PWTIDX01";
/// Index layout version. A bump invalidates old indexes (pages are rebuilt).
const MANIFEST_VERSION: u64 = 1;

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
}

/// Content identity of the client terrain sources for one world.
///
/// Every input that can change the decoded grid enters the digest: the whole
/// `world.trn2` header, each subterrain name, and the full bytes of every
/// `*.t2bk`. The resulting hex string names the warm-start index, so a client
/// patch, a repacked archive or a different world cannot resolve to the wrong
/// cached grid. Reading the sources is one sequential pass; it replaces the
/// per-block zlib decode, XOR plane encode, SHA-256 and write of a cold build.
pub fn source_identity(header: &[u8], dir: &Path, names: &[String]) -> Result<String, String> {
    let mut hash = Sha256::new();
    hash.update(b"PWTSRC01");
    hash.update((header.len() as u64).to_le_bytes());
    hash.update(header);
    // The same TRN2/T2BK corpus can be copied into two instance folders. Those
    // are separate coordinate spaces, so their warm caches must not collide.
    hash.update(b"PWTMAPDIR");
    let map_dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    hash.update(map_dir.to_string_lossy().as_bytes());
    for name in names {
        let bytes =
            fs::read(dir.join(format!("{name}.t2bk"))).map_err(|e| format!("{name}: {e}"))?;
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
    }
    Ok(hex(hash.finalize().as_slice()))
}

fn take_u64(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let end = pos.checked_add(8)?;
    let value = u64::from_le_bytes(bytes.get(*pos..end)?.try_into().ok()?);
    *pos = end;
    Some(value)
}

fn take_u32(bytes: &[u8], pos: &mut usize) -> Option<u32> {
    let end = pos.checked_add(4)?;
    let value = u32::from_le_bytes(bytes.get(*pos..end)?.try_into().ok()?);
    *pos = end;
    Some(value)
}

type Manifest = (Vec<Record>, String, Vec<u8>);

fn parse_manifest(bytes: &[u8], width: usize, height: usize, side: usize) -> Option<Manifest> {
    if bytes.len() < MANIFEST_MAGIC.len() + 32 + 8 {
        return None;
    }
    // The trailing SHA-256 covers everything before it: a truncated or edited
    // index is rejected instead of being trusted.
    let split = bytes.len() - 32;
    if Sha256::digest(&bytes[..split]).as_slice() != &bytes[split..] {
        return None;
    }
    let body = &bytes[..split];
    if &body[..MANIFEST_MAGIC.len()] != MANIFEST_MAGIC.as_slice() {
        return None;
    }
    let mut pos = MANIFEST_MAGIC.len();
    if take_u64(body, &mut pos)? != MANIFEST_VERSION {
        return None;
    }
    if take_u64(body, &mut pos)? != width as u64
        || take_u64(body, &mut pos)? != height as u64
        || take_u64(body, &mut pos)? != side as u64
    {
        return None;
    }
    let pages = take_u64(body, &mut pos)?;
    let name_len = take_u32(body, &mut pos)? as usize;
    let name_end = pos.checked_add(name_len)?;
    let pages_name = std::str::from_utf8(body.get(pos..name_end)?)
        .ok()?
        .to_string();
    pos = name_end;
    if !safe_name(&pages_name) || !pages_name.ends_with(".pages") {
        return None;
    }
    let digest_len = take_u32(body, &mut pos)? as usize;
    if digest_len != 32 {
        return None;
    }
    let digest_end = pos.checked_add(digest_len)?;
    let pages_digest = body.get(pos..digest_end)?.to_vec();
    pos = digest_end;
    let expected = (width / side).checked_mul(height / side)?;
    if pages != expected as u64 {
        return None;
    }
    let mut records = Vec::with_capacity(expected);
    for _ in 0..expected {
        let offset = take_u64(body, &mut pos)?;
        let size = take_u64(body, &mut pos)?;
        let hash_end = pos.checked_add(32)?;
        let mut hash = [0u8; 32];
        hash.copy_from_slice(body.get(pos..hash_end)?);
        pos = hash_end;
        if size == 0 || size > usize::MAX as u64 {
            return None;
        }
        records.push(Record {
            offset,
            size: size as usize,
            hash,
        });
    }
    if pos != body.len() {
        return None;
    }
    Some((records, pages_name, pages_digest))
}

fn write_manifest(
    root: &Path,
    identity: &str,
    width: usize,
    height: usize,
    side: usize,
    pages_name: &str,
    pages_digest: &[u8],
    records: &[Record],
) -> Result<(), String> {
    if !safe_name(identity) || !safe_name(pages_name) || !pages_name.ends_with(".pages") {
        return Err("invalid terrain cache names".into());
    }
    let mut out = MANIFEST_MAGIC.to_vec();
    out.extend_from_slice(&MANIFEST_VERSION.to_le_bytes());
    for value in [
        width as u64,
        height as u64,
        side as u64,
        records.len() as u64,
    ] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&(pages_name.len() as u32).to_le_bytes());
    out.extend_from_slice(pages_name.as_bytes());
    out.extend_from_slice(&(pages_digest.len() as u32).to_le_bytes());
    out.extend_from_slice(pages_digest);
    for record in records {
        out.extend_from_slice(&record.offset.to_le_bytes());
        out.extend_from_slice(&(record.size as u64).to_le_bytes());
        out.extend_from_slice(&record.hash);
    }
    let digest = Sha256::digest(&out);
    out.extend_from_slice(digest.as_slice());
    let target = root.join(format!("{identity}.index"));
    let temp = root.join(format!("{identity}.index.tmp"));
    fs::write(&temp, &out).map_err(|e| format!("{}: {e}", temp.display()))?;
    if fs::rename(&temp, &target).is_err() {
        // Windows refuses to replace an existing file; retry once behind a
        // removal. The pages object this index names is immutable and already
        // verified, so either outcome is safe.
        let _ = fs::remove_file(&target);
        if let Err(e) = fs::rename(&temp, &target) {
            let _ = fs::remove_file(&temp);
            return Err(format!("{}: {e}", target.display()));
        }
    }
    Ok(())
}

/// Open a previously built pages object by source identity.
///
/// This is the warm start: when the index matches the client sources, the
/// caller can skip reading and decoding every T2BK block, re-encoding the XOR
/// planes, hashing the result and rewriting the pages file. Everything is
/// re-verified first — the index digest, the dimensions, every record's extent
/// against the file, and the pages file's own SHA-256 — and any failure returns
/// `Ok(None)` so the caller rebuilds from the client instead of drawing damaged
/// terrain.
pub fn restore(
    root: &Path,
    identity: &str,
    width: usize,
    height: usize,
    side: usize,
    budget: usize,
) -> Result<Option<(Heights, Normals)>, String> {
    if !safe_name(identity) {
        return Ok(None);
    }
    let index = root.join(format!("{identity}.index"));
    let bytes = match fs::read(&index) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", index.display())),
    };
    let Some((records, pages_name, pages_digest)) = parse_manifest(&bytes, width, height, side)
    else {
        eprintln!(
            "Terrain cache index {} does not match this world; rebuilding from the client",
            index.display()
        );
        return Ok(None);
    };
    let pages_path = root.join(&pages_name);
    let mut file = match File::open(&pages_path) {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 65536];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return Ok(None),
        }
    }
    if hasher.finalize().as_slice() != pages_digest.as_slice() {
        eprintln!(
            "Terrain cache {} failed its content check; rebuilding from the client",
            pages_path.display()
        );
        return Ok(None);
    }
    let file_len = file.metadata().map_err(|e| e.to_string())?.len();
    if records.iter().any(|record| {
        record
            .offset
            .checked_add(record.size as u64)
            .is_none_or(|end| end > file_len)
    }) {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let pages = records.len();
    let store = Arc::new(Store {
        len: width * height,
        width,
        side,
        data: Data::Disk(Mutex::new(Disk {
            file,
            path: pages_path,
            side,
            records,
            hot: Hot {
                pages: HashMap::new(),
                clock: 0,
                stats: Stats {
                    budget_bytes: budget,
                    ..Stats::default()
                },
            },
        })),
    });
    eprintln!(
        "Terrain cache: warm start, {pages} verified blocks reused from {} (source decode and recompression skipped), decoded hot-set budget {:.2} MiB",
        pages_name,
        budget as f64 / 1048576.
    );
    Ok(Some((Heights(Arc::clone(&store)), Normals(store))))
}

pub fn budget() -> usize {
    std::env::var("PW_TERRAIN_RAM_MIB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|n| n.clamp(1, 256) * 1024 * 1024)
        .unwrap_or(DEFAULT_BUDGET)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "pw-terrain-pages-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn height(x: usize, y: usize) -> f32 {
        ((x as i32 - 37) * 127 + (y as i32 - 13) * 31) as f32 / 17.
    }
    fn normal(x: usize, y: usize) -> [i8; 3] {
        [x as i8, (x ^ y) as i8, y as i8]
    }
    fn fixture(root: &Path, budget: usize, change: bool) -> (Heights, Normals) {
        let mut b = Builder::new(root, 32, 24, 4).unwrap();
        // Source order need not be the final row-major grid order.
        for bx in (0..8).rev() {
            for by in 0..6 {
                let mut h = Vec::new();
                let mut n = Vec::new();
                for y in 0..4 {
                    for x in 0..4 {
                        h.push(height(bx * 4 + x, by * 4 + y) + if change { 1. } else { 0. });
                        n.push(normal(bx * 4 + x, by * 4 + y));
                    }
                }
                b.block(bx, by, &h, &n).unwrap();
            }
        }
        b.finish(budget).unwrap()
    }
    #[test]
    fn codec_preserves_all_float_and_signed_normal_bits() {
        let bits = [
            0, 0x80000000, 0x7fc12345, 0x7f800000, 0xff800000, 0x3f800001, 0x00000001, 0xffffffff,
        ];
        let h: Vec<_> = (0..16)
            .map(|i| f32::from_bits(bits[i % bits.len()]))
            .collect();
        let n: Vec<_> = (0..16).map(|i| [i as i8 - 8, -128, 127]).collect();
        let p = decode(&encode(&h, &n, 4), 4);
        assert_eq!(
            p.heights.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
            h.iter().map(|f| f.to_bits()).collect::<Vec<_>>()
        );
        assert_eq!(p.normals, n);
    }
    #[test]
    fn source_identity_separates_identical_bytes_in_different_map_folders() {
        let a = Temp::new();
        let b = Temp::new();
        let first = source_identity(b"same-header", &a.0, &[]).unwrap();
        let second = source_identity(b"same-header", &b.0, &[]).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn paging_is_exact_across_edges_evictions_and_shared_views() {
        let dir = Temp::new();
        let (h, n) = fixture(&dir.0, 4 * 4 * 7 * 2, false);
        let mut reader = h.reader();
        for pass in 0..3 {
            for i in 0..32 * 24 {
                let index = (i * 173 + pass * 47) % (32 * 24);
                let x = index % 32;
                let y = index / 32;
                assert_eq!(reader.height(index).to_bits(), height(x, y).to_bits());
                assert_eq!(reader.normal(index), normal(x, y));
            }
        }
        assert_eq!(n.at(0), normal(0, 0));
        let stats = h.stats();
        assert!(stats.evictions > 100);
        assert!(stats.reads > 100);
        assert!(stats.peak_bytes <= stats.budget_bytes);
        assert!(stats.resident_bytes <= 224);
        h.trim();
        assert_eq!(h.stats().resident_bytes, 0);
        assert_eq!(h.at(767).to_bits(), height(31, 23).to_bits());
    }
    #[test]
    fn current_block_cursor_does_not_lock_or_read_again() {
        let dir = Temp::new();
        let (h, _) = fixture(&dir.0, 224, false);
        let mut r = h.reader();
        for _ in 0..100 {
            for y in 0..4 {
                for x in 0..4 {
                    r.height(y * 32 + x);
                    r.normal(y * 32 + x);
                }
            }
        }
        assert_eq!(h.stats().reads, 1);
    }
    #[test]
    fn cache_identity_reuse_source_changes_and_corruption_repair() {
        let dir = Temp::new();
        drop(fixture(&dir.0, 224, false));
        let paths = || {
            fs::read_dir(&dir.0)
                .unwrap()
                .map(|p| p.unwrap().path())
                .collect::<Vec<_>>()
        };
        assert_eq!(paths().len(), 1);
        drop(fixture(&dir.0, 224, false));
        assert_eq!(paths().len(), 1);
        let path = paths().pop().unwrap();
        fs::write(&path, b"damaged").unwrap();
        let (h, n) = fixture(&dir.0, 224, false);
        assert_eq!(h.at(0), height(0, 0));
        drop((h, n));
        let (h, _) = fixture(&dir.0, 224, true);
        assert_eq!(h.at(0), height(0, 0) + 1.);
        assert_eq!(paths().len(), 2);
    }
    #[test]
    fn damaged_page_never_silently_becomes_geometry() {
        let dir = Temp::new();
        let (h, _) = fixture(&dir.0, 224, false);
        let Data::Disk(d) = &h.0.data else { panic!() };
        let d = d.lock().unwrap();
        let path = d.path.clone();
        let offset = d.records[0].offset;
        drop(d);
        let mut f = OpenOptions::new().write(true).open(path).unwrap();
        f.seek(SeekFrom::Start(offset)).unwrap();
        f.write_all(&[0xff; 8]).unwrap();
        drop(f);
        assert!(h.reader().page(0).is_err());
    }
    #[test]
    fn shared_readers_stay_exact_under_concurrent_eviction() {
        let dir = Temp::new();
        let (h, _) = fixture(&dir.0, 224, false);
        std::thread::scope(|s| {
            for thread in 0..4 {
                let h = h.clone();
                s.spawn(move || {
                    let mut r = h.reader();
                    for i in 0..500 {
                        let index = (i * 31 + thread * 47) % 768;
                        assert_eq!(
                            r.height(index).to_bits(),
                            height(index % 32, index / 32).to_bits()
                        );
                    }
                });
            }
        });
        assert!(h.stats().peak_bytes <= 224);
    }
    /// Fill a paged fixture under an explicit source identity.
    fn fixture_with_identity(root: &Path, identity: &str) -> (Heights, Normals) {
        let mut b = Builder::new(root, 32, 24, 4)
            .unwrap()
            .with_identity(identity);
        for bx in 0..8 {
            for by in 0..6 {
                let mut h = Vec::new();
                let mut n = Vec::new();
                for y in 0..4 {
                    for x in 0..4 {
                        h.push(height(bx * 4 + x, by * 4 + y));
                        n.push(normal(bx * 4 + x, by * 4 + y));
                    }
                }
                b.block(bx, by, &h, &n).unwrap();
            }
        }
        b.finish(224).unwrap()
    }

    fn assert_grid(heights: &Heights, normals: &Normals) {
        let mut reader = heights.reader();
        for y in 0..24 {
            for x in 0..32 {
                let index = y * 32 + x;
                assert_eq!(reader.height(index).to_bits(), height(x, y).to_bits());
                assert_eq!(reader.normal(index), normal(x, y));
                assert_eq!(normals.at(index), normal(x, y));
            }
        }
    }

    #[test]
    fn resident_mode_keeps_every_cell_without_a_disk_object() {
        let cells = 32 * 24;
        let mut b = Builder::new_resident(32, 24, 4).unwrap();
        // Source order need not be the final row-major order, exactly as the
        // client's subterrain files are not ordered by block position.
        for bx in (0..8).rev() {
            for by in 0..6 {
                let mut h = Vec::new();
                let mut n = Vec::new();
                for y in 0..4 {
                    for x in 0..4 {
                        h.push(height(bx * 4 + x, by * 4 + y));
                        n.push(normal(bx * 4 + x, by * 4 + y));
                    }
                }
                b.block(bx, by, &h, &n).unwrap();
            }
        }
        let (h, n) = b.finish(0).unwrap();
        assert!(h.is_resident());
        assert_grid(&h, &n);
        // No eviction, no read accounting: the grid is simply held.
        let stats = h.stats();
        assert_eq!(stats.reads, 0);
        assert_eq!(stats.evictions, 0);
        assert_eq!(
            stats.resident_bytes,
            cells * 7 + std::mem::size_of::<Resident>()
        );
        // An exhaustive scan must not release it, unlike the paged hot set.
        h.trim();
        assert_eq!(
            h.stats().resident_bytes,
            cells * 7 + std::mem::size_of::<Resident>()
        );
        assert_grid(&h, &n);
    }

    #[test]
    fn resident_mode_rejects_duplicate_and_incomplete_grids() {
        let mut b = Builder::new_resident(8, 8, 4).unwrap();
        b.block(0, 0, &[1.; 16], &[[0; 3]; 16]).unwrap();
        assert!(b.block(0, 0, &[1.; 16], &[[0; 3]; 16]).is_err());
        assert!(b.block(2, 0, &[1.; 16], &[[0; 3]; 16]).is_err());
        assert!(b.finish(0).is_err());
        assert!(Builder::new_resident(7, 8, 4).is_err());
        assert!(Builder::new_resident(0, 8, 4).is_err());
    }

    #[test]
    fn warm_start_reuses_the_index_and_checks_every_input() {
        let dir = Temp::new();
        let (h, n) = fixture_with_identity(&dir.0, "source-one");
        assert_grid(&h, &n);
        drop(h);
        drop(n);
        assert!(
            dir.0.join("source-one.index").is_file(),
            "the index must be published next to the pages object"
        );
        // A second start resolves the same grid through the index alone.
        let (h, n) = restore(&dir.0, "source-one", 32, 24, 4, 224)
            .unwrap()
            .expect("warm start");
        assert_grid(&h, &n);
        assert!(!h.is_resident(), "a warm start still pages");
        // A different source identity must never resolve to this grid.
        assert!(restore(&dir.0, "source-two", 32, 24, 4, 224)
            .unwrap()
            .is_none());
        // Dimensions that disagree with the index are rejected.
        assert!(restore(&dir.0, "source-one", 16, 24, 4, 224)
            .unwrap()
            .is_none());
        // An unsafe identity never touches the disk.
        assert!(restore(&dir.0, "../source", 32, 24, 4, 224)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_damaged_index_or_pages_object_is_never_silently_used() {
        let dir = Temp::new();
        let _ = fixture_with_identity(&dir.0, "source-damaged");
        let index = dir.0.join("source-damaged.index");
        let original = fs::read(&index).unwrap();

        // One flipped byte in the index body fails its own SHA-256.
        let mut edited = original.clone();
        edited[16] ^= 0xff;
        fs::write(&index, &edited).unwrap();
        assert!(restore(&dir.0, "source-damaged", 32, 24, 4, 224)
            .unwrap()
            .is_none());

        // Truncation is rejected too.
        fs::write(&index, &original[..original.len() / 2]).unwrap();
        assert!(restore(&dir.0, "source-damaged", 32, 24, 4, 224)
            .unwrap()
            .is_none());
        fs::write(&index, &original).unwrap();

        // A good index over a damaged pages object fails the stream hash.
        let pages = fs::read_dir(&dir.0)
            .unwrap()
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                (path.extension().and_then(|e| e.to_str()) == Some("pages")).then_some(path)
            })
            .next()
            .expect("pages object");
        let mut bytes = fs::read(&pages).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&pages, &bytes).unwrap();
        assert!(restore(&dir.0, "source-damaged", 32, 24, 4, 224)
            .unwrap()
            .is_none());
        // And the intact object still restores once it is written back.
        bytes[last] ^= 0xff;
        fs::write(&pages, &bytes).unwrap();
        assert!(restore(&dir.0, "source-damaged", 32, 24, 4, 224)
            .unwrap()
            .is_some());
    }

    #[test]
    fn source_identity_covers_the_header_names_and_bytes() {
        let dir = Temp::new();
        let header = b"world.trn2 header";
        let sub = dir.0.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let write = |bytes: &[u8]| fs::write(sub.join("a.t2bk"), bytes).unwrap();
        let names = vec!["a".to_string()];
        write(b"first payload");
        let one = source_identity(header, &sub, &names).unwrap();
        assert_eq!(one.len(), 64, "sha-256 as hex");
        assert!(one.chars().all(|c| c.is_ascii_hexdigit()));
        write(b"second payload");
        let two = source_identity(header, &sub, &names).unwrap();
        assert_ne!(one, two, "the subterrain bytes must change the identity");
        write(b"first payload");
        assert_eq!(source_identity(header, &sub, &names).unwrap(), one);
        // The header and the name list are inputs as well.
        assert_ne!(
            source_identity(b"another header", &sub, &names).unwrap(),
            one
        );
        // A different name list is a different world even with identical bytes.
        fs::write(sub.join("b.t2bk"), b"first payload").unwrap();
        assert_ne!(
            source_identity(header, &sub, &["b".to_string()]).unwrap(),
            one
        );
        assert!(source_identity(header, &sub, &["missing".to_string()]).is_err());
    }

    #[test]
    fn incomplete_duplicate_and_out_of_bounds_builds_fail_cleanly() {
        let dir = Temp::new();
        let mut b = Builder::new(&dir.0, 8, 8, 4).unwrap();
        assert!(b.block(2, 0, &[0.; 16], &[[0; 3]; 16]).is_err());
        b.block(0, 0, &[0.; 16], &[[0; 3]; 16]).unwrap();
        assert!(b.block(0, 0, &[0.; 16], &[[0; 3]; 16]).is_err());
        assert!(b.finish(224).is_err());
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
        assert!(Builder::new(&dir.0, 7, 8, 4).is_err());
    }
}
