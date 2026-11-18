//! Reader for the retail client's v2 PCK directory and entries.
//! Layout informed by GameForge's GPL-2.0-or-later Angelica importer.
use flate2::Status;
use flate2::{Decompress, FlushDecompress};
#[cfg(windows)]
#[path = "direct_storage.rs"]
pub(crate) mod direct_storage;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
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

/// Trailer and entry-record sizes for each package revision the client opens.
///
/// The client dispatches on the version word at `nOffset - 4`
/// (`AFilePackage::Open`). All three revisions keep the same shape: a trailer
/// holding the guards, the entry offset and the count, followed backwards by a
/// run of length-prefixed entry records. Only the sizes differ, so the reader
/// walks them with one routine.
///
/// | revision     | `sizeof` header | header at  | `sizeof` entry |
/// |--------------|-----------------|------------|----------------|
/// | `0x00020001` | 272             | `end - 280` | 272           |
/// | `0x00020002` | 272             | `end - 280` | 272           |
/// | `0x00020003` | 280             | `end - 288` | 280           |
///
/// `guardByte1` sits at `+268` in all three; `0x00020003` adds an `UNKNOWN`
/// dword after it and widens `dwOffset` to `UINT64`. The count is *not* a
/// header field: the client reads it from `end - 8`, in the eight bytes between
/// the trailer and the version word (for `0x00020003` that lands in the
/// alignment padding after the struct, which is why the trailer looks eight
/// bytes longer than it is).
#[derive(Clone, Copy)]
struct Revision {
    version: u32,
    /// `sizeof(FILEHEADER*)` — the trailer alone, without the count or the
    /// version word that follow it.
    header: usize,
    /// `sizeof(FILEENTRY*)`, which is also the size of an uncompressed entry
    /// record: `szFileName[260]`, `dwOffset`, `dwLength`,
    /// `dwCompressedLength`, `iAccessCnt`, plus `Zalupa0`/`Zalupa1` in the
    /// newest revision, which widens `dwOffset` to 64 bits.
    entry: usize,
    /// Where `dwOffset` starts inside an entry record.
    offset_at: usize,
    /// Width of `dwOffset`.
    offset_width: usize,
    /// Where `dwLength` starts.
    size_at: usize,
    /// Where `dwCompressedLength` starts.
    compressed_at: usize,
    /// Where `dwFlags` starts inside the trailer.
    flags_at: usize,
    /// Where `szDescription` starts inside the trailer.
    description_at: usize,
    /// Where `guardByte1` starts inside the trailer.
    guard_byte1: usize,
}

/// One table serves all three revisions; only the sizes differ.
///
/// | revision     | trailer | entry | `dwOffset` | `dwLength` | `dwCompressedLength` |
/// |--------------|---------|-------|------------|------------|----------------------|
/// | `0x00020001` | 272     | 276   | `+260`/4   | `+264`     | `+268`               |
/// | `0x00020002` | 272     | 276   | `+260`/4   | `+264`     | `+268`               |
/// | `0x00020003` | 280     | 288   | `+264`/8   | `+272`     | `+276`               |
///
/// The trailer's own fields move in `0x00020003` because `dwEntryOffset` is
/// 64-bit there, which pushes everything after it eight bytes along:
/// `dwFlags` `+12`→`+16`, `szDescription` `+16`→`+20`, `guardByte1`
/// `+268`→`+272`. It then adds an `UNKNOWN` dword and pads to 280.
///
/// The count is *not* a trailer field in any revision: the client reads it from
/// `end - 8`, the four bytes between the trailer and the version word at
/// `end - 4`.
const REVISIONS: [Revision; 3] = [
    Revision {
        version: 0x2_0003,
        header: 280,
        entry: 288,
        offset_at: 264,
        offset_width: 8,
        size_at: 272,
        compressed_at: 276,
        flags_at: 16,
        description_at: 20,
        guard_byte1: 272,
    },
    Revision {
        version: 0x2_0002,
        header: 272,
        entry: 276,
        offset_at: 260,
        offset_width: 4,
        size_at: 264,
        compressed_at: 268,
        flags_at: 12,
        description_at: 16,
        guard_byte1: 268,
    },
    Revision {
        version: 0x2_0001,
        header: 272,
        entry: 276,
        offset_at: 260,
        offset_width: 4,
        size_at: 264,
        compressed_at: 268,
        flags_at: 12,
        description_at: 16,
        guard_byte1: 268,
    },
];

/// Validate the 12-byte prefix every package starts with.
///
/// `continuation` names the `.pkx` half of a split archive when the caller
/// found one beside the `.pck`, so a declared size larger than this file is
/// reported as the split it is. Without that evidence the same bytes are a
/// corrupt size word, and calling it a split would send a reader looking for a
/// file that does not exist.
fn safe_header(safe: &[u8], size: u64, continuation: Option<&Path>) -> Result<(), String> {
    if u32le(safe, 0)? != 0x4dca23ef || u32le(safe, 8)? != 0x56a089b7 {
        return Err("unsupported PCK safe header".into());
    }
    let end = u32le(safe, 4)? as u64;
    if end < 292 {
        return Err("bad package end offset".into());
    }
    if end > size {
        // The client reads a package split at `MAX_FILE_PACKAGE` together with
        // its `.pkx` continuation (`models.pck` + `models.pkx` in the 1.5.2
        // client is one 2.93 GB archive). This reader is handed a single open
        // file, so it names the split instead of calling the archive corrupt.
        return Err(match continuation {
            Some(path) => format!(
                "split package: the header declares {end} bytes but this file holds {size}; \
                 the rest is in `{}`, which this reader does not open",
                path.display()
            ),
            None => format!(
                "bad package end offset: the header declares {end} bytes but this file holds {size}"
            ),
        });
    }
    Ok(())
}

/// The offset the trailer sits at: the prefix's declared size when it fits on
/// disk, otherwise the end of the `.pck` file itself.
fn package_end(f: &mut File) -> Result<u64, String> {
    let size = f.metadata().map_err(|e| e.to_string())?.len();
    let mut safe = [0u8; 12];
    f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    f.read_exact(&mut safe).map_err(|e| e.to_string())?;
    let declared = u32le(&safe, 4)? as u64;
    Ok(if declared <= size && declared >= 292 {
        declared
    } else {
        size
    })
}

/// The container revision, read the way the client reads it, and the trailer
/// that belongs to it.
///
/// The client seeks to `end - 4` for the version word *before* it knows how
/// large the trailer is, then reads the trailer at `end - (header + 4 + 4)`
/// because the file count sits between the trailer and the version word. The
/// two reads are reproduced here in that order.
fn read_trailer(f: &mut File, end: u64) -> Result<(Revision, usize, Vec<u8>), String> {
    let version = u32le(&at(f, end - 4, 4)?, 0)?;
    let Some(rev) = REVISIONS.iter().find(|r| r.version == version) else {
        return Err(format!("unsupported PCK version {version:08x}"));
    };
    // The client reads the trailer at `nOffset - (sizeof(header) + 4 + 4)` and
    // the count at `nOffset - 8`: the two reads overlap the same eight bytes
    // only when the struct's tail padding is counted, so both are done from the
    // header start here instead.
    let header_start = end
        .checked_sub((rev.header + 8) as u64)
        .ok_or("bad package end offset")?;
    let count = u32le(&at(f, end - 8, 4)?, 0)? as usize;
    let header = at(f, header_start, rev.header)?;
    if u32le(&header, 0)? != 0xfdfdfeee || u32le(&header, rev.guard_byte1)? != 0xf00dbeef {
        return Err("unsupported PCK guard keys".into());
    }
    // The client refuses a package whose description lacks "lica File
    // Package" (`strstr(m_header.szDescription, "lica File Package")`), so a
    // file that passes the guards but not this is not a package at all. Saying
    // "unsupported version" for it would send the reader after a version that
    // does not exist.
    let description = &header[rev.description_at..rev.description_at + 252];
    let stop = description
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(description.len());
    let description = String::from_utf8_lossy(&description[..stop]);
    if !description.contains("lica File Package") {
        return Err("not an Angelica file package (description tag missing)".into());
    }
    Ok((*rev, count, header))
}
pub fn directory(f: &mut File) -> Result<Vec<Entry>, String> {
    let size = f.metadata().map_err(|e| e.to_string())?.len();
    if size < 292 {
        return Err("archive too small".into());
    }
    let safe = at(f, 0, 12)?;
    safe_header(&safe, size, None)?;
    let end = package_end(f)?;
    let (rev, count, header) = read_trailer(f, end)?;
    if u32le(&header, rev.flags_at)? & 0x80000000 != 0 {
        return Err("encrypted package unsupported".into());
    }
    if count > 1_000_000 {
        return Err("entry count exceeds limit".into());
    }
    // The client unmasks the entry offset in place (`dwEntryOffset ^=
    // AFPCK_MASKDWORD`) and seeks there; the trailer follows the table, so a
    // table that starts at or past it is corrupt.
    let pos = (u32le(&header, 8)? ^ MASK) as u64;
    let table_end = end - (rev.header + 8) as u64;
    if pos >= table_end {
        return Err("entry table overlaps footer".into());
    }
    let table = at(f, pos, (table_end - pos) as usize)?;
    read_entry_table(&table, count, &rev, end, size)
}

/// Walk the length-prefixed run of entry records the client reads one at a
/// time.
///
/// Every revision frames a record as two masked dwords — the stored length and
/// a check word — followed by that many bytes. A record whose stored length
/// equals the record size is stored verbatim; anything else is a zlib stream of
/// it, which the client decodes with `Uncompress` and, when the stored copy is
/// no smaller, re-compresses (`AFilePackage::Open`). Both cases occur in retail
/// packs, so both are handled here.
fn read_entry_table(
    table: &[u8],
    count: usize,
    rev: &Revision,
    end: u64,
    size: u64,
) -> Result<Vec<Entry>, String> {
    let mut cursor = 0usize;
    let mut decomp = Decompress::new(true);
    let mut buf = [0u8; 288];
    let mut result = Vec::with_capacity(count);
    for _ in 0..count {
        if cursor + 8 > table.len() {
            return Err("entry table overlaps footer".into());
        }
        let h = &table[cursor..cursor + 8];
        let len = (u32le(h, 0)? ^ MASK) as usize;
        // The client compares the decoded words before touching the payload, so
        // a mismatch is a framing error rather than a bad record.
        if u32le(h, 4)? ^ MASK ^ CHECK != len as u32 {
            return Err("invalid entry table record (check word)".into());
        }
        if len == 0 || len > rev.entry || cursor + 8 + len > table.len() {
            return Err("invalid entry table record (length)".into());
        }
        let raw = &table[cursor + 8..cursor + 8 + len];
        let data: &[u8] = if len == rev.entry {
            raw
        } else {
            decomp.reset(true);
            decomp
                .decompress(raw, &mut buf[..rev.entry], FlushDecompress::Finish)
                .map_err(|e| e.to_string())?;
            if decomp.total_out() != rev.entry as u64 {
                let n = decomp.total_out();
                return Err(format!("unexpected entry record size: {n}"));
            }
            &buf[..rev.entry]
        };
        let zero = data[..260].iter().position(|&x| x == 0).unwrap_or(260);
        let (decoded, _, _) = encoding_rs::GBK.decode(&data[..zero]);
        let name = decoded.replace('\\', "/");
        // `dwOffset` is `DWORD` in the older revisions and `UINT64` in
        // `0x00020003`; the client copies it into a `FILEENTRY` whose field is
        // only a `DWORD`, so it truncates there too.
        let offset = if rev.offset_width == 8 {
            u64::from_le_bytes(data[rev.offset_at..rev.offset_at + 8].try_into().unwrap()) as u32
        } else {
            u32le(data, rev.offset_at)?
        };
        let file_size = u32le(data, rev.size_at)?;
        let compressed = u32le(data, rev.compressed_at)?;
        if offset as u64 + compressed as u64 > end
            || offset as u64 + compressed as u64 > size
            || file_size > 512 * 1024 * 1024
            || compressed > file_size
        {
            return Err(format!("invalid entry bounds: {name}"));
        }
        result.push(Entry {
            name,
            offset,
            size: file_size,
            compressed,
        });
        cursor += 8 + len;
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
    // Written only by the DirectStorage path, which exists on Windows alone.
    #[cfg_attr(not(windows), allow(unused_mut))]
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

    /// Build a package the way `AFilePackage::AppendFile` does, for any of the
    /// three revisions: a 12-byte prefix, payloads, then the entry table and the
    /// trailer.
    ///
    /// `compress_entries` writes each entry record as a zlib stream, which is
    /// what the client produces when compressing a record actually shrinks it;
    /// otherwise the record is stored verbatim at full record size.
    fn package(rev: u32, payloads: &[(&str, &[u8])], compress_entries: bool) -> Vec<u8> {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;

        // Sizes and field offsets from the engine structs: `FILEHEADER*` is the
        // trailer alone, and one entry record carries the trailing
        // `iAccessCnt` as well as the fields the reader uses.
        let (header_size, entry_size, offset_at, offset_width, size_at) = match rev {
            0x0002_0003 => (280usize, 288usize, 264usize, 8usize, 272usize),
            0x0002_0002 | 0x0002_0001 => (272, 276, 260, 4, 264),
            other => panic!("unsupported test revision {other:08x}"),
        };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x4dca23efu32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // patched below
        bytes.extend_from_slice(&0x56a089b7u32.to_le_bytes());
        let mut offsets = Vec::new();
        for (_, data) in payloads {
            offsets.push(bytes.len() as u32);
            bytes.extend_from_slice(data);
        }
        let table = bytes.len() as u32;
        for (index, (name, data)) in payloads.iter().enumerate() {
            let mut record = vec![0u8; entry_size];
            let name = name.as_bytes();
            record[..name.len()].copy_from_slice(name);
            let length_at = size_at;
            if offset_width == 8 {
                record[offset_at..offset_at + 8]
                    .copy_from_slice(&(offsets[index] as u64).to_le_bytes());
            } else {
                record[offset_at..offset_at + 4].copy_from_slice(&offsets[index].to_le_bytes());
            }
            record[length_at..length_at + 4].copy_from_slice(&(data.len() as u32).to_le_bytes());
            record[length_at + 4..length_at + 8]
                .copy_from_slice(&(data.len() as u32).to_le_bytes());

            let stored: Vec<u8> = if compress_entries {
                let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
                encoder.write_all(&record).unwrap();
                encoder.finish().unwrap()
            } else {
                record.clone()
            };
            // The client writes the record itself only when compressing did not
            // make it smaller.
            let stored = if stored.len() >= entry_size {
                record.clone()
            } else {
                stored
            };
            bytes.extend_from_slice(&((stored.len() as u32) ^ MASK).to_le_bytes());
            bytes.extend_from_slice(&((stored.len() as u32) ^ MASK ^ CHECK).to_le_bytes());
            bytes.extend_from_slice(&stored);
        }
        let mut header = vec![0u8; header_size];
        header[0..4].copy_from_slice(&0xfdfdfeeeu32.to_le_bytes());
        header[4..8].copy_from_slice(&rev.to_le_bytes());
        header[8..12].copy_from_slice(&(table ^ MASK).to_le_bytes());
        // The trailer's fields move between revisions because `0x00020003`
        // widens `dwEntryOffset`; the offsets below are the engine's own.
        let (flags_at, description_at, guard_byte1) = if header_size == 280 {
            (16usize, 20usize, 272usize)
        } else {
            (12, 16, 268)
        };
        let tag = b"Angelica File Package, Perfect World Co. Ltd.";
        header[description_at..description_at + tag.len()].copy_from_slice(tag);
        let _ = flags_at;
        header[guard_byte1..guard_byte1 + 4].copy_from_slice(&0xf00dbeefu32.to_le_bytes());
        bytes.extend_from_slice(&header);
        // The client reads the count from `end - 8`, between the trailer and
        // the version word.
        bytes.extend_from_slice(&(payloads.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&rev.to_le_bytes());
        let total = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&total.to_le_bytes());
        bytes
    }

    fn write_temp(bytes: &[u8], tag: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "pw-pck-{tag}-{}-{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// The field offsets in [`REVISIONS`] are taken from the engine's structs,
    /// and every one of them was wrong in the first draft of this reader: the
    /// trailer's `dwFlags` and `szDescription` shift by eight bytes in
    /// `0x00020003` because `dwEntryOffset` is 64-bit, `guardByte1` moves with
    /// them, and the entry record is 288 bytes rather than the 280 the field
    /// list suggests (the trailing `Zalupa1`/`iAccessCnt` count). Pin them so a
    /// later tidy-up has to argue with this test rather than with a retail pack.
    ///
    /// Re-deriving from `AFilePackage.cpp`/`.h` on MSVC x86 (where `UINT64` is
    /// 4-aligned) gives:
    ///
    /// * `FILEHEADER_0x00020003` — 280 B: `dwFlags +16`, `szDescription +20`,
    ///   `guardByte1 +272`, `UNKNOWN +276`.
    /// * `FILEENTRY_0x00020003` — 288 B: `dwOffset +264`, `dwLength +272`,
    ///   `dwCompressedLength +276`.
    /// * `FILEHEADER` / `FILEENTRY` — 272 / 276 B: `dwFlags +12`,
    ///   `szDescription +16`, `guardByte1 +268`; `dwOffset +260`,
    ///   `dwLength +264`, `dwCompressedLength +268`.
    #[test]
    fn revision_layout_matches_the_engine_structs() {
        let v3 = REVISIONS.iter().find(|r| r.version == 0x2_0003).unwrap();
        assert_eq!(
            (
                v3.header,
                v3.entry,
                v3.offset_at,
                v3.offset_width,
                v3.size_at,
                v3.compressed_at,
                v3.flags_at,
                v3.description_at,
                v3.guard_byte1
            ),
            (280, 288, 264, 8, 272, 276, 16, 20, 272),
            "FILEHEADER/FILEENTRY_0x00020003"
        );
        for v2 in REVISIONS.iter().filter(|r| r.version != 0x2_0003) {
            assert_eq!(
                (
                    v2.header,
                    v2.entry,
                    v2.offset_at,
                    v2.offset_width,
                    v2.size_at,
                    v2.compressed_at,
                    v2.flags_at,
                    v2.description_at,
                    v2.guard_byte1
                ),
                (272, 276, 260, 4, 264, 268, 12, 16, 268),
                "FILEHEADER/FILEENTRY for {:#08x}",
                v2.version
            );
        }
        // The trailer is the last thing before the count and version word.
        for rev in REVISIONS {
            assert!(
                rev.description_at + 252 <= rev.header,
                "szDescription must fit inside the trailer"
            );
            assert!(rev.guard_byte1 + 4 <= rev.header, "guardByte1 must fit");
            assert!(
                rev.compressed_at + 4 <= rev.entry,
                "the entry must be whole"
            );
        }
    }

    /// `0x00020003` moves `guardByte1` from `+268` to `+272`, widens
    /// `dwOffset` to `UINT64`, adds an `UNKNOWN` dword, reads the trailer from
    /// `end-288` rather than `end-280`, and takes the count from `+276` rather
    /// than `+268`. Reading it with the older revision's constants therefore
    /// fails the guard check outright, which is exactly what the reader did
    /// before this revision was added: it refused the package with
    /// `unsupported PCK version 00020003`.
    #[test]
    fn reads_the_64_bit_offset_package_revision() {
        let bytes = package(
            0x0002_0003,
            &[("a/b.txt", b"hello"), ("music\\theme.mp3", b"world")],
            false,
        );
        let path = write_temp(&bytes, "v3");
        let mut file = std::fs::File::open(&path).unwrap();
        let entries = directory(&mut file).unwrap();
        assert_eq!(
            entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["a/b.txt", "music/theme.mp3"],
            "the GBK name and its separator normalisation have to survive"
        );
        assert_eq!(read_entry(&mut file, &entries[0]).unwrap(), b"hello");
        assert_eq!(read_entry(&mut file, &entries[1]).unwrap(), b"world");
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    /// The client stores an entry record verbatim only when compressing it did
    /// not help; otherwise the record is a zlib stream of itself. Both framings
    /// appear in retail packs, and a reader that assumes a fixed-size record
    /// desynchronises on the second entry of a compressed table.
    #[test]
    fn reads_a_compressed_entry_table() {
        let bytes = package(
            0x0002_0003,
            &[
                ("gfx/\\u4eba\\u7269/\\u6280\\u80fd/a.gfx", &vec![7u8; 4000]),
                ("short.txt", b"tail"),
            ],
            true,
        );
        let path = write_temp(&bytes, "v3zip");
        let mut file = std::fs::File::open(&path).unwrap();
        let entries = directory(&mut file).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].name, "short.txt");
        assert_eq!(read_entry(&mut file, &entries[0]).unwrap(), vec![7u8; 4000]);
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    /// The two older revisions share a layout, so both must still read — the
    /// branch that added `0x00020003` must not have moved the goalposts for
    /// the packs the shipped clients actually carry.
    #[test]
    fn reads_both_older_revisions() {
        for rev in [0x0002_0002u32, 0x0002_0001] {
            let bytes = package(rev, &[("a.txt", b"one"), ("b/c.txt", b"two")], false);
            let path = write_temp(&bytes, "old");
            let mut file = std::fs::File::open(&path).unwrap();
            let entries = directory(&mut file).unwrap();
            assert_eq!(entries.len(), 2, "revision {rev:08x}");
            assert_eq!(read_entry(&mut file, &entries[1]).unwrap(), b"two");
            drop(file);
            std::fs::remove_file(path).unwrap();
        }
    }

    /// The description tag is the client's own package test: a file with the
    /// right guards and a plausible trailer that lacks it is not a package, and
    /// saying "unsupported PCK version" for it would send a reader looking for
    /// a version it does not have.
    #[test]
    fn a_trailer_without_the_package_description_is_refused_as_not_a_package() {
        let mut bytes = package(0x0002_0003, &[("a.txt", b"x")], false);
        let description = bytes.len() - 280 - 4 + 16;
        bytes[description..description + 40].fill(b' ');
        let path = write_temp(&bytes, "nodesc");
        let mut file = std::fs::File::open(&path).unwrap();
        let error = directory(&mut file).unwrap_err();
        drop(file);
        std::fs::remove_file(path).unwrap();
        assert!(
            error.contains("not an Angelica file package"),
            "unexpected error: {error}"
        );
    }

    /// A package whose header declares more bytes than the file holds has been
    /// split at `MAX_FILE_PACKAGE`; the continuation lives in `.pkx`. With that
    /// continuation present the reader must name the split, because "corrupt
    /// archive" would be the wrong thing to go looking for — the archive is
    /// fine and the second half is simply not read.
    #[test]
    fn a_split_package_is_named_as_a_split_when_its_continuation_is_present() {
        let mut bytes = package(0x0002_0003, &[("a.txt", b"x")], false);
        let declared = bytes.len() as u32 + 4096;
        bytes[4..8].copy_from_slice(&declared.to_le_bytes());
        let path = write_temp(&bytes, "split");
        let mut file = std::fs::File::open(&path).unwrap();
        safe_header(
            &bytes[..12],
            bytes.len() as u64,
            Some(Path::new("models.pkx")),
        )
        .unwrap_err()
        .contains("split package")
        .then_some(())
        .expect("a declared size past the file, with a .pkx present, is a split");
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    /// The same bytes with **no** `.pkx` beside them are a corrupt size word,
    /// not a split: there is no continuation to point at, and naming one would
    /// invent a file.
    #[test]
    fn a_declared_size_past_the_file_without_a_continuation_is_corruption() {
        let mut bytes = package(0x0002_0003, &[("a.txt", b"x")], false);
        let declared = bytes.len() as u32 + 4096;
        bytes[4..8].copy_from_slice(&declared.to_le_bytes());
        let path = write_temp(&bytes, "nosplit");
        let mut file = std::fs::File::open(&path).unwrap();
        let error = directory(&mut file).unwrap_err();
        drop(file);
        std::fs::remove_file(path).unwrap();
        assert!(
            error.contains("bad package end offset"),
            "unexpected error: {error}"
        );
        assert!(!error.contains("split"), "unexpected error: {error}");
    }
}
