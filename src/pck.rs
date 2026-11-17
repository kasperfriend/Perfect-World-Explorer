//! Reader for the retail client's v2 PCK directory and entries.
//! Layout informed by GameForge's GPL-2.0-or-later Angelica importer.
use flate2::read::ZlibDecoder;
use flate2::{Decompress, FlushDecompress};
#[cfg(windows)]
#[path = "direct_storage.rs"]
pub(crate) mod direct_storage;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};
const MASK: u32 = 0xa8937462;
const CHECK: u32 = 0x59374231;

/// A PCK and its optional `.pkx` overflow file as one seekable logical package.
/// Angelica stores payload bytes across the pair contiguously; directory offsets
/// and the package-end field address that combined byte stream.
pub struct PackageFile {
    path: PathBuf,
    pck: File,
    pkx: Option<File>,
    pck_len: u64,
    pkx_len: u64,
    pos: u64,
}

impl PackageFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        let pck = File::open(path)?;
        let pck_len = pck.metadata()?.len();
        let lower_pkx = path.with_extension("pkx");
        let upper_pkx = path.with_extension("PKX");
        let pkx_path = if lower_pkx.is_file() {
            Some(lower_pkx)
        } else if upper_pkx.is_file() {
            Some(upper_pkx)
        } else {
            None
        };
        let (pkx, pkx_len) = if let Some(pkx_path) = pkx_path {
            let pkx = File::open(&pkx_path).map_err(|error| {
                io::Error::new(error.kind(), format!("{}: {error}", pkx_path.display()))
            })?;
            let len = pkx.metadata()?.len();
            (Some(pkx), len)
        } else {
            (None, 0)
        };
        Ok(Self {
            path: path.to_path_buf(),
            pck,
            pkx,
            pck_len,
            pkx_len,
            pos: 0,
        })
    }

    fn total_len(&self) -> u64 {
        self.pck_len.saturating_add(self.pkx_len)
    }

    pub fn uses_pkx(&self) -> bool {
        self.pkx.is_some()
    }
}

impl Read for PackageFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.total_len() {
            return Ok(0);
        }
        if self.pos < self.pck_len {
            self.pck.seek(SeekFrom::Start(self.pos))?;
            let span = (self.pck_len - self.pos).min(buf.len() as u64) as usize;
            let read = self.pck.read(&mut buf[..span])?;
            self.pos += read as u64;
            if read != span || read == buf.len() {
                return Ok(read);
            }
            return self.read(&mut buf[read..]).map(|next| read + next);
        }
        let Some(pkx) = self.pkx.as_mut() else {
            return Ok(0);
        };
        pkx.seek(SeekFrom::Start(self.pos - self.pck_len))?;
        let read = pkx.read(buf)?;
        self.pos += read as u64;
        Ok(read)
    }
}

impl Seek for PackageFile {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let next = match from {
            SeekFrom::Start(pos) => pos as i128,
            SeekFrom::Current(delta) => self.pos as i128 + delta as i128,
            SeekFrom::End(delta) => self.total_len() as i128 + delta as i128,
        };
        if next < 0 || next > u64::MAX as i128 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid seek in {}", self.path.display()),
            ));
        }
        self.pos = next as u64;
        Ok(self.pos)
    }
}

fn u32le(b: &[u8], p: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4).ok_or("truncated")?.try_into().unwrap(),
    ))
}
fn at<R: Read + Seek>(f: &mut R, p: u64, n: usize) -> Result<Vec<u8>, String> {
    let mut b = vec![0; n];
    // Small directory/header reads are cheaper through the regular file API.
    // Larger archive payloads can use the optional DirectStorage system-memory
    // queue when the user built against Microsoft's SDK and installed runtime.
    // Every request retains the original positional-read fallback.
    #[cfg(windows)]
    if n >= 64 * 1024 && direct_storage::read_range(f, p, &mut b) {
        return Ok(b);
    }
    f.seek(SeekFrom::Start(p))
        .and_then(|_| f.read_exact(&mut b))
        .map_err(|e| e.to_string())?;
    Ok(b)
}
fn inflate(bytes: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(expected);
    ZlibDecoder::new(bytes)
        .take((expected + 1) as u64)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    if out.len() != expected {
        return Err(format!(
            "bad decompressed size: {} != {expected}",
            out.len()
        ));
    }
    Ok(out)
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub offset: u32,
    pub size: u32,
    pub compressed: u32,
}
pub fn directory<R: Read + Seek>(f: &mut R) -> Result<Vec<Entry>, String> {
    let size = f.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    if size < 292 {
        return Err("archive too small".into());
    }
    let safe = at(f, 0, 12)?;
    if u32le(&safe, 0)? != 0x4dca23ef || u32le(&safe, 8)? != 0x56a089b7 {
        return Err("unsupported PCK safe header".into());
    }
    let end = u32le(&safe, 4)? as u64;
    if end > size {
        return Err(format!(
            "package end offset {end} exceeds available bytes {size} (missing or truncated .pkx companion if split)"
        ));
    }
    if end < 292 {
        return Err("bad package end offset".into());
    }
    let header = at(f, end - 280, 280)?;
    if u32le(&header, 0)? != 0xfdfdfeee || u32le(&header, 268)? != 0xf00dbeef {
        return Err("unsupported PCK guard keys".into());
    }
    let version = u32le(&header, 276)?;
    if version != 0x20002 && version != 0x20001 {
        return Err(format!("unsupported PCK version {version:08x}"));
    };
    if u32le(&header, 12)? & 0x80000000 != 0 {
        return Err("encrypted package unsupported".into());
    }
    let count = u32le(&header, 272)? as usize;
    if count > 1_000_000 {
        return Err("entry count exceeds limit".into());
    }
    let pos = (u32le(&header, 8)? ^ MASK) as u64;
    let table_end = end - 280;
    if pos > table_end {
        return Err("entry table overlaps footer".into());
    }
    let table = at(f, pos, (table_end - pos) as usize)?;
    let mut cursor = 0usize;
    let mut decomp = Decompress::new(true);
    let mut buf = [0u8; 276];
    let mut result = Vec::with_capacity(count);
    for _ in 0..count {
        if cursor + 8 > table.len() {
            return Err("entry table overlaps footer".into());
        }
        let h = &table[cursor..cursor + 8];
        let len = (u32le(h, 0)? ^ MASK) as usize;
        if u32le(h, 4)? ^ MASK ^ CHECK != len as u32
            || len == 0
            || len > 276
            || cursor + 8 + len > table.len()
        {
            return Err("invalid entry table record".into());
        }
        let raw = &table[cursor + 8..cursor + 8 + len];
        let data: &[u8] = if len == 276 {
            raw
        } else {
            decomp.reset(true);
            decomp
                .decompress(raw, &mut buf, FlushDecompress::Finish)
                .map_err(|e| e.to_string())?;
            if decomp.total_out() != 276 {
                let n = decomp.total_out();
                return Err(format!("unexpected entry record size: {n}"));
            }
            &buf
        };
        let zero = data[..260].iter().position(|&x| x == 0).unwrap_or(260);
        let (decoded, _, _) = encoding_rs::GBK.decode(&data[..zero]);
        let name = decoded.replace('\\', "/");
        let offset = u32le(&data, 260)?;
        let usize_ = u32le(&data, 264)?;
        let compressed = u32le(&data, 268)?;
        if offset as u64 + compressed as u64 > end
            || usize_ > 512 * 1024 * 1024
            || compressed > usize_
        {
            return Err(format!("invalid entry bounds: {name}"));
        }
        result.push(Entry {
            name,
            offset,
            size: usize_,
            compressed,
        });
        cursor += 8 + len;
    }
    Ok(result)
}

pub fn read_entry<R: Read + Seek>(f: &mut R, e: &Entry) -> Result<Vec<u8>, String> {
    if e.compressed < e.size {
        let compressed = at(f, e.offset as u64, e.compressed as usize)?;
        let mut out = Vec::with_capacity(e.size as usize);
        ZlibDecoder::new(compressed.as_slice())
            .take(e.size as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|err| err.to_string())?;
        if out.len() != e.size as usize {
            return Err(format!(
                "bad decompressed size: {} != {}",
                out.len(),
                e.size
            ));
        }
        Ok(out)
    } else {
        at(f, e.offset as u64, e.compressed as usize)
    }
}

/// Read a bounded set of entries from one archive. Large reads are queued
/// together on DirectStorage when available; ordinary positioned I/O remains
/// the correctness-preserving fallback for every failed batch.
pub fn read_entries_at(
    f: &File,
    entries: &[&Entry],
) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    let mut raw: Vec<Vec<u8>> = entries
        .iter()
        .map(|entry| vec![0u8; entry.compressed as usize])
        .collect();
    let mut direct_storage_done = vec![false; entries.len()];

    #[cfg(windows)]
    {
        const DIRECT_STORAGE_MIN: usize = 64 * 1024;
        const BATCH_CAPACITY: usize = 64;
        let mut ranges = Vec::with_capacity(BATCH_CAPACITY);
        let mut range_indices = Vec::with_capacity(BATCH_CAPACITY);
        for (index, (entry, bytes)) in entries.iter().zip(raw.iter_mut()).enumerate() {
            if bytes.len() < DIRECT_STORAGE_MIN {
                continue;
            }
            ranges.push((entry.offset as u64, bytes.as_mut_slice()));
            range_indices.push(index);
            if ranges.len() == BATCH_CAPACITY {
                if direct_storage::read_ranges(f, &mut ranges) {
                    for &index in &range_indices {
                        direct_storage_done[index] = true;
                    }
                }
                ranges.clear();
                range_indices.clear();
            }
        }
        if !ranges.is_empty() && direct_storage::read_ranges(f, &mut ranges) {
            for &index in &range_indices {
                direct_storage_done[index] = true;
            }
        }
    }

    for ((entry, bytes), used_direct_storage) in
        entries.iter().zip(raw.iter_mut()).zip(direct_storage_done)
    {
        if used_direct_storage {
            continue;
        }
        let mut read = 0usize;
        while read < bytes.len() {
            let off = entry.offset as u64 + read as u64;
            #[cfg(windows)]
            let n = std::os::windows::fs::FileExt::seek_read(f, &mut bytes[read..], off)?;
            #[cfg(unix)]
            let n = std::os::unix::fs::FileExt::read_at(f, &mut bytes[read..], off)?;
            if n == 0 {
                return Err(format!("unexpected EOF in PCK entry {}", entry.name).into());
            }
            read += n;
        }
    }

    entries
        .iter()
        .zip(raw)
        .map(|(entry, bytes)| {
            if entry.compressed < entry.size {
                let out = inflate(&bytes, entry.size as usize)?;
                if out.len() != entry.size as usize {
                    return Err(
                        format!("bad decompressed size: {} != {}", out.len(), entry.size).into(),
                    );
                }
                Ok(out)
            } else {
                Ok(bytes)
            }
        })
        .collect()
}

pub fn read_entry_at(f: &File, e: &Entry) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    read_entries_at(f, &[e])?
        .pop()
        .ok_or_else(|| "empty PCK read batch".into())
}

/// Environment variable that turns a shipped-corpus skip into a failure.
///
/// The tests that read the retail data (`gfx.pck`, `world.ecwld`, the loose
/// `element` directory) have to skip on a machine that does not stage them: CI
/// stages none of it, and the archives are 97 MB and more, so they are not in the
/// repository. Printing `SKIPPED: …` is better than returning a bare `ok`, but it
/// still means a corpus assertion can rot for months while every run is green —
/// which is exactly what happened here, where four expectations about the shipped
/// waterfall, beams and portal were written, reviewed and pushed without ever being
/// executed.
///
/// `PWE_CORPUS=require` turns every skip into a failure, so a machine that does
/// stage the archives can run the suite as a gate:
///
/// ```sh
/// PWE_CORPUS=require cargo test --bin perfect-world-explorer
/// ```
///
/// It is deliberately all-or-nothing: a machine missing one archive fails with the
/// path it is missing rather than quietly testing the rest, because the point of the
/// switch is to know that the shipped data was checked, not which fraction of it.
///
/// The helper lives here, beside the archive reader, because every module that has
/// a corpus test already reaches this one — a module of its own would have to be
/// declared in each of the 29 `src/bin` crates that include those modules. It is
/// `cfg(test)` so no release build carries it.
#[cfg(test)]
pub const CORPUS_ENV: &str = "PWE_CORPUS";

/// Whether the caller asked for the corpus suite to be run as a gate.
#[cfg(test)]
pub fn corpus_required() -> bool {
    corpus_required_with(std::env::var(CORPUS_ENV).ok().as_deref())
}

/// [`corpus_required`] over an already-read value, so the rule can be tested
/// without mutating the process environment.
#[cfg(test)]
pub fn corpus_required_with(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.eq_ignore_ascii_case("require"))
}

/// Report a missing shipped archive, and fail when the corpus is required.
///
/// `what` names the archive the way a reader would stage it, e.g.
/// `` `google_drive/gfx.pck` ``.
#[cfg(test)]
pub fn corpus_absent(what: &str) {
    corpus_absent_with(corpus_required(), what);
}

/// [`corpus_absent`] with the decision already made.
#[cfg(test)]
pub fn corpus_absent_with(required: bool, what: &str) {
    if required {
        panic!("{CORPUS_ENV}=require, but this machine has no {what}");
    }
    eprintln!(
        "SKIPPED: no {what} on this machine; this test proves nothing about the shipped data \
         (set {CORPUS_ENV}=require to make this a failure)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default has to be "off": CI stages no archives, so a corpus test that
    /// failed there would fail every run for a reason the change cannot fix.
    #[test]
    fn only_the_word_require_turns_the_corpus_gate_on() {
        assert!(!corpus_required_with(None));
        assert!(!corpus_required_with(Some("")));
        assert!(!corpus_required_with(Some("0")));
        assert!(!corpus_required_with(Some("skip")));
        assert!(corpus_required_with(Some("require")));
        assert!(corpus_required_with(Some("REQUIRE")));
        assert!(corpus_required_with(Some("Require")));
    }

    /// With the gate on, a missing archive is a failure that names the path —
    /// that message is the whole point, because it tells the operator which
    /// archive to stage.
    #[test]
    #[should_panic(expected = "PWE_CORPUS=require, but this machine has no `google_drive/gfx.pck`")]
    fn a_required_corpus_fails_instead_of_skipping() {
        corpus_absent_with(true, "`google_drive/gfx.pck`");
    }

    /// With the gate off it prints and returns, which is what every run without a
    /// staged archive does.
    #[test]
    fn an_optional_corpus_skips() {
        corpus_absent_with(false, "`google_drive/gfx.pck`");
    }

    use std::io::Write;

    fn one_entry_package(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(payload).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut record = vec![0u8; 276];
        record[..name.len()].copy_from_slice(name.as_bytes());
        record[260..264].copy_from_slice(&12u32.to_le_bytes());
        record[264..268].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        record[268..272].copy_from_slice(&(compressed.len() as u32).to_le_bytes());
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x4dca23efu32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // patched with logical length
        bytes.extend_from_slice(&0x56a089b7u32.to_le_bytes());
        bytes.extend_from_slice(&compressed);
        let directory_at = bytes.len() as u32;
        bytes.extend_from_slice(&(276u32 ^ MASK).to_le_bytes());
        bytes.extend_from_slice(&(276u32 ^ MASK ^ CHECK).to_le_bytes());
        bytes.extend_from_slice(&record);
        let end = bytes.len() as u32 + 280;
        let mut footer = vec![0u8; 280];
        footer[0..4].copy_from_slice(&0xfdfdfeeeu32.to_le_bytes());
        footer[8..12].copy_from_slice(&(directory_at ^ MASK).to_le_bytes());
        footer[268..272].copy_from_slice(&0xf00dbeefu32.to_le_bytes());
        footer[272..276].copy_from_slice(&1u32.to_le_bytes());
        footer[276..280].copy_from_slice(&0x20002u32.to_le_bytes());
        bytes.extend_from_slice(&footer);
        bytes[4..8].copy_from_slice(&end.to_le_bytes());
        bytes
    }

    #[test]
    fn split_pck_and_pkx_are_read_as_one_logical_archive() {
        let root = std::env::temp_dir().join(format!(
            "pw-pck-split-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let pck_path = root.join("litmodels.pck");
        let pkx_path = root.join("litmodels.pkx");
        let payload = vec![0x42; 4096];
        let bytes = one_entry_package("litmodels/world/1/example.bmd", &payload);
        let split = 19; // split inside both the payload and the logical stream
        std::fs::write(&pck_path, &bytes[..split]).unwrap();
        std::fs::write(&pkx_path, &bytes[split..]).unwrap();

        let mut package = PackageFile::open(&pck_path).unwrap();
        let entries = directory(&mut package).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "litmodels/world/1/example.bmd");
        assert!(entries[0].compressed < entries[0].size);
        assert_eq!(read_entry(&mut package, &entries[0]).unwrap(), payload);

        drop(package);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn truncated_split_archive_names_the_missing_pkx_companion() {
        let root = std::env::temp_dir().join(format!(
            "pw-pck-no-pkx-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let pck_path = root.join("litmodels.pck");
        let bytes = one_entry_package("litmodels/world/1/example.bmd", &vec![0x42; 4096]);
        let truncated_at = bytes.len() - 100;
        std::fs::write(&pck_path, &bytes[..truncated_at]).unwrap();
        let mut package = PackageFile::open(&pck_path).unwrap();
        let error = directory(&mut package).unwrap_err();
        assert!(error.contains("missing or truncated .pkx companion"));
        drop(package);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_archive_batch_preserves_independent_ranges_and_decompression() {
        let first = vec![37u8; 128 * 1024];
        let second = b"a separate compressed entry".repeat(257);
        let third: Vec<u8> = (0..96 * 1024).map(|i| (i % 251) as u8).collect();
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&second).unwrap();
        let packed_second = encoder.finish().unwrap();
        let path = std::env::temp_dir().join(format!(
            "pw-pck-batch-{}-{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let first_offset = 4096u32;
        file.write_all(&vec![0; first_offset as usize]).unwrap();
        file.write_all(&first).unwrap();
        let second_offset = first_offset + first.len() as u32 + 512;
        file.write_all(&vec![0; 512]).unwrap();
        file.write_all(&packed_second).unwrap();
        let third_offset = second_offset + packed_second.len() as u32 + 1024;
        file.write_all(&vec![0; 1024]).unwrap();
        file.write_all(&third).unwrap();
        file.flush().unwrap();

        let entries = [
            Entry {
                name: "first".into(),
                offset: first_offset,
                size: first.len() as u32,
                compressed: first.len() as u32,
            },
            Entry {
                name: "second".into(),
                offset: second_offset,
                size: second.len() as u32,
                compressed: packed_second.len() as u32,
            },
            Entry {
                name: "third".into(),
                offset: third_offset,
                size: third.len() as u32,
                compressed: third.len() as u32,
            },
        ];
        let references: Vec<_> = entries.iter().collect();
        let decoded = read_entries_at(&file, &references).unwrap();
        drop(file);
        std::fs::remove_file(path).unwrap();
        assert_eq!(decoded, vec![first, second, third]);
    }

    #[test]
    fn bounded_streaming_decode_matches_the_original_member() {
        let expected = vec![37u8; 128 * 1024];
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&expected).unwrap();
        let packed = encoder.finish().unwrap();
        let path = std::env::temp_dir().join(format!(
            "pw-pck-{}-{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        f.write_all(&[0; 17]).unwrap();
        f.write_all(&packed).unwrap();
        f.write_all(&[99; 100]).unwrap(); // next member is not part of the stream
        let mut entry = Entry {
            name: "test".into(),
            offset: 17,
            size: expected.len() as u32,
            compressed: packed.len() as u32,
        };
        let decoded = read_entry(&mut f, &entry);
        entry.size -= 1;
        let invalid = read_entry(&mut f, &entry);
        drop(f);
        std::fs::remove_file(path).unwrap();
        assert_eq!(decoded.unwrap(), expected);
        assert!(invalid.is_err());
    }
}
