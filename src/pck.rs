//! Reader for the retail client's v2 PCK directory and entries.
//! Layout informed by GameForge's GPL-2.0-or-later Angelica importer.
use flate2::Status;
use flate2::{Decompress, FlushDecompress};
#[cfg(windows)]
#[path = "direct_storage.rs"]
pub(crate) mod direct_storage;

/// Platform-neutral view of the DirectStorage telemetry.
///
/// `direct_storage` only exists on Windows, but the in-viewer loading overlay is
/// compiled on every host and still has to print the same readout. This re-export
/// keeps one call site in the overlay instead of a `cfg` in the middle of the
/// paint code; on a host without DirectStorage the counters read zero and the
/// state line says so, which is exactly what a Windows build without the SDK
/// prints.
pub(crate) mod readout {
    /// The counters the loading UI displays.
    #[derive(Clone, Copy, Debug, Default)]
    pub(crate) struct Stats {
        pub attempts: u64,
        pub successes: u64,
        pub fallbacks: u64,
        pub bytes: u64,
        pub active: usize,
    }

    #[cfg(windows)]
    pub(crate) fn stats() -> Stats {
        let stats = super::direct_storage::read_stats();
        Stats {
            attempts: stats.attempts,
            successes: stats.successes,
            fallbacks: stats.fallbacks,
            bytes: stats.bytes,
            active: stats.active,
        }
    }

    #[cfg(not(windows))]
    pub(crate) fn stats() -> Stats {
        Stats::default()
    }

    /// One sentence describing what DirectStorage is doing right now.
    #[cfg(windows)]
    pub(crate) fn state_line(stats: Stats) -> String {
        super::direct_storage::state_line(super::direct_storage::ReadStats {
            attempts: stats.attempts,
            successes: stats.successes,
            fallbacks: stats.fallbacks,
            bytes: stats.bytes,
            active: stats.active,
        })
    }

    #[cfg(not(windows))]
    pub(crate) fn state_line(_stats: Stats) -> String {
        "DirectStorage is a Windows feature; standard file I/O is in use".into()
    }
}
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};
const MASK: u32 = 0xa8937462;
const CHECK: u32 = 0x59374231;
fn u32le(b: &[u8], p: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4).ok_or("truncated")?.try_into().unwrap(),
    ))
}
fn at(f: &mut File, p: u64, n: usize) -> Result<Vec<u8>, String> {
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
pub(crate) fn inflate(raw: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    thread_local! { static INFLATE_DECOMP: std::cell::RefCell<Decompress> = std::cell::RefCell::new(Decompress::new(true)); }
    INFLATE_DECOMP.with(|cell| {
        let mut decomp = cell.borrow_mut();
        decomp.reset(true);
        // One spare byte distinguishes an overlong payload from an exact fit,
        // including the empty-stream case. Never grow beyond the declared bound.
        let cap = expected.checked_add(1).ok_or("zlib size overflow")?;
        let mut out = Vec::with_capacity(cap);
        let status = decomp
            .decompress_vec(raw, &mut out, FlushDecompress::Finish)
            .map_err(|e| e.to_string())?;
        if out.len() != expected {
            return Err(format!(
                "bad decompressed size: {} != {expected}",
                out.len()
            ));
        }
        if status != Status::StreamEnd {
            return Err("truncated zlib stream".into());
        }
        Ok(out)
    })
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub offset: u32,
    pub size: u32,
    pub compressed: u32,
}
#[derive(Hash, PartialEq, Eq)]
struct DirCacheKey {
    size: u64,
    mtime_secs: u64,
    mtime_nanos: u32,
    end: u64,
    pos: u64,
    count: usize,
    header_hash: [u8; 32],
}

fn dir_cache() -> &'static std::sync::Mutex<std::collections::HashMap<DirCacheKey, std::sync::Arc<Vec<Entry>>>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<DirCacheKey, std::sync::Arc<Vec<Entry>>>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn decode_entry_record(
    raw: &[u8],
    end: u64,
    decomp: &mut Decompress,
    buf: &mut [u8; 276],
) -> Result<Entry, String> {
    let data: &[u8] = if raw.len() == 276 {
        raw
    } else {
        decomp.reset(true);
        decomp
            .decompress(raw, buf, FlushDecompress::Finish)
            .map_err(|e| e.to_string())?;
        if decomp.total_out() != 276 {
            let n = decomp.total_out();
            return Err(format!("unexpected entry record size: {n}"));
        }
        buf.as_slice()
    };
    let zero = data[..260].iter().position(|&x| x == 0).unwrap_or(260);
    let name_bytes = &data[..zero];
    let mut name = if name_bytes.is_ascii() {
        // Fast path: ASCII filenames skip GBK state-machine decoding.
        String::from_utf8(name_bytes.to_vec()).unwrap_or_default()
    } else {
        encoding_rs::GBK.decode(name_bytes).0.into_owned()
    };
    if name.as_bytes().contains(&b'\\') {
        // SAFETY: replacing ASCII '\\' with ASCII '/' preserves UTF-8 validity.
        unsafe {
            for b in name.as_bytes_mut() {
                if *b == b'\\' {
                    *b = b'/';
                }
            }
        }
    }
    let offset = u32le(data, 260)?;
    let usize_ = u32le(data, 264)?;
    let compressed = u32le(data, 268)?;
    if offset as u64 + compressed as u64 > end || usize_ > 512 * 1024 * 1024 || compressed > usize_
    {
        return Err(format!("invalid entry bounds: {name}"));
    }
    Ok(Entry {
        name,
        offset,
        size: usize_,
        compressed,
    })
}

pub fn directory(f: &mut File) -> Result<Vec<Entry>, String> {
    let meta = f.metadata().map_err(|e| e.to_string())?;
    let size = meta.len();
    if size < 292 {
        return Err("archive too small".into());
    }
    let safe = at(f, 0, 12)?;
    if u32le(&safe, 0)? != 0x4dca23ef || u32le(&safe, 8)? != 0x56a089b7 {
        return Err("unsupported PCK safe header".into());
    }
    let end = u32le(&safe, 4)? as u64;
    if end > size || end < 292 {
        return Err("bad package end offset (split packages unsupported)".into());
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
    // Check process-wide PCK directory cache before reading/decompressing the
    // entry table. Avoids re-parsing multi-hundred-thousand-entry archives like
    // textures.pck (8-10x per world load) and litmodels.pck on world switches
    // and whole-world toggles.
    let cache_key = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|dur| {
            use sha2::Digest as _;
            let mut h = sha2::Sha256::new();
            h.update(&safe);
            h.update(&header);
            DirCacheKey {
                size,
                mtime_secs: dur.as_secs(),
                mtime_nanos: dur.subsec_nanos(),
                end,
                pos,
                count,
                header_hash: h.finalize().into(),
            }
        });
    if let Some(ref key) = cache_key {
        if let Ok(guard) = dir_cache().lock() {
            if let Some(cached) = guard.get(key) {
                return Ok((**cached).clone());
            }
        }
    }
    let table = at(f, pos, (table_end - pos) as usize)?;
    // Fast linear scan of 8-byte record headers to collect record slices,
    // allowing parallel zlib + GBK decoding across all CPU cores.
    let mut slices: Vec<(usize, usize)> = Vec::with_capacity(count);
    let mut cursor = 0usize;
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
        slices.push((cursor + 8, len));
        cursor += 8 + len;
    }
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(16);
    let result = if workers > 1 && count >= 512 {
        let chunk_size = (count + workers - 1) / workers;
        let table_ref = &table;
        let chunks: Result<Vec<Vec<Entry>>, String> = std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(workers);
            for chunk in slices.chunks(chunk_size) {
                handles.push(s.spawn(move || -> Result<Vec<Entry>, String> {
                    let mut decomp = Decompress::new(true);
                    let mut buf = [0u8; 276];
                    let mut out = Vec::with_capacity(chunk.len());
                    for &(start, len) in chunk {
                        out.push(decode_entry_record(
                            &table_ref[start..start + len],
                            end,
                            &mut decomp,
                            &mut buf,
                        )?);
                    }
                    Ok(out)
                }));
            }
            let mut all = Vec::with_capacity(handles.len());
            for h in handles {
                all.push(h.join().unwrap()?);
            }
            Ok(all)
        });
        let mut flat = Vec::with_capacity(count);
        for chunk in chunks? {
            flat.extend(chunk);
        }
        flat
    } else {
        let mut decomp = Decompress::new(true);
        let mut buf = [0u8; 276];
        let mut out = Vec::with_capacity(count);
        for &(start, len) in &slices {
            out.push(decode_entry_record(
                &table[start..start + len],
                end,
                &mut decomp,
                &mut buf,
            )?);
        }
        out
    };
    if let Some(key) = cache_key {
        if let Ok(mut guard) = dir_cache().lock() {
            // Cap cache to 32 distinct archives so tests creating temporary PCKs
            // cannot grow memory unboundedly.
            if guard.len() >= 32 {
                guard.clear();
            }
            guard.insert(key, std::sync::Arc::new(result.clone()));
        }
    }
    Ok(result)
}

pub fn read_entry(f: &mut File, e: &Entry) -> Result<Vec<u8>, String> {
    if e.compressed < e.size {
        let compressed = at(f, e.offset as u64, e.compressed as usize)?;
        inflate(&compressed, e.size as usize)
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
                inflate(&bytes, entry.size as usize).map_err(Into::into)
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
    #[test]
    fn bounded_inflate_resets_after_bad_streams() {
        for input in [Vec::new(), vec![42; 128 * 1024]] {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(&input).unwrap();
            let raw = encoder.finish().unwrap();
            assert!(inflate(&raw[..raw.len() - 1], input.len()).is_err());
            assert!(inflate(&raw, input.len() + 1).is_err());
            if !input.is_empty() {
                assert!(inflate(&raw, 1).is_err());
            }
            assert_eq!(inflate(&raw, input.len()).unwrap(), input);
        }
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
