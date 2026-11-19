//! Reader for the retail client's v2 PCK directory and entries.
//! Layout informed by GameForge's GPL-2.0-or-later Angelica importer.
use flate2::Status;
use flate2::{Decompress, FlushDecompress};
#[cfg(windows)]
#[path = "direct_storage.rs"]
pub(crate) mod direct_storage;
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
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
/// Positional read that needs no `&mut File`, so a shared handle can be read
/// from several places at once (and from both halves of a split package).
fn at_shared(f: &File, p: u64, n: usize) -> Result<Vec<u8>, String> {
    let mut b = vec![0; n];
    let mut read = 0usize;
    while read < n {
        #[cfg(windows)]
        let step = std::os::windows::fs::FileExt::seek_read(f, &mut b[read..], p + read as u64);
        #[cfg(unix)]
        let step = std::os::unix::fs::FileExt::read_at(f, &mut b[read..], p + read as u64);
        let step = step.map_err(|e| e.to_string())?;
        if step == 0 {
            return Err("truncated".into());
        }
        read += step;
    }
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
    /// Byte offset from the start of the package, not of one file.
    pub offset: u64,
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
pub(crate) struct Revision {
    pub(crate) version: u32,
    /// `sizeof(FILEHEADER*)` — the trailer alone, without the count or the
    /// version word that follow it.
    pub(crate) header: usize,
    /// `sizeof(FILEENTRY*)`, which is also the size of an uncompressed entry
    /// record: `szFileName[260]`, `dwOffset`, `dwLength`,
    /// `dwCompressedLength`, `iAccessCnt`, plus `Zalupa0`/`Zalupa1` in the
    /// newest revision, which widens `dwOffset` to 64 bits.
    pub(crate) entry: usize,
    /// Where `dwOffset` starts inside an entry record.
    pub(crate) offset_at: usize,
    /// Width of `dwOffset`.
    pub(crate) offset_width: usize,
    /// Where `dwLength` starts.
    pub(crate) size_at: usize,
    /// Where `dwCompressedLength` starts.
    pub(crate) compressed_at: usize,
    /// Where `dwFlags` starts inside the trailer.
    pub(crate) flags_at: usize,
    /// Where `szDescription` starts inside the trailer.
    description_at: usize,
    /// Where `guardByte1` starts inside the trailer.
    pub(crate) guard_byte1: usize,
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
pub(crate) const REVISIONS: [Revision; 3] = [
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

/// The guard pair `guardByte0` / `guardByte1` that every revision's trailer
/// carries.
///
/// Retail packages from the 1.8.4 client write this pair. It is one of *two*
/// shipped pairs, which is why it is not the only constant here: see
/// [`ANGELICA_GUARD_0`]. The mistake to avoid is deduplicating two genuinely
/// different values into one, not spelling the same value twice.
pub(crate) const GUARD_0: u32 = 0xfdfdfeee;
pub(crate) const GUARD_1: u32 = 0xf00dbeef;

/// The guard pair a 1.5.2-era Angelica container writes instead.
///
/// This is **not** a legacy oddity to be tolerated: it is what every 1.5.2
/// `.pck` carries. Measured on `configs.pck` (67 entries) and `grasses.pck`
/// (103 entries), both revision `0x00020002`, both `0xa508bdc4`/`0x853567c8`
/// at trailer `+0` and `+268`. Only the 1.8.4 pack, revision `0x00020003`,
/// carries `0xfdfdfeee`/`0xf00dbeef`.
///
/// An earlier consolidation removed this pair on the grounds that "it appears
/// in no staged archive". The archive had not been staged; the claim was
/// really "I did not look", and it broke every 1.5.2 package. Two pairs, each
/// measured, each named.
pub(crate) const ANGELICA_GUARD_0: u32 = 0xa508_bdc4;
pub(crate) const ANGELICA_GUARD_1: u32 = 0x8535_67c8;

/// `dwFlags` bit 31: the package's entry records and payloads are enciphered.
///
/// Named rather than spelled `0x80000000` at the refusal, because the *bit* is
/// a fact about the format and the refusal is a fact about this build. The
/// client clears this bit itself when it opens such a package; this reader has
/// no deciphering step, so it stops here -- but it stops by *name*.
pub(crate) const ENCRYPTED_FLAG: u32 = 0x8000_0000;

/// The package is a well-formed retail package that this build cannot read.
///
/// It is deliberately distinguishable from a corrupt archive. Every other
/// rejection says "these bytes are not a package"; this one says "these bytes
/// are a package, and here is the feature the reader lacks". A survey that
/// cannot tell the two apart reports a shipped archive as broken, which is how
/// an unsupported feature hides for years -- so the census and the harnesses
/// match this text and count it as a skip, not a failure.
pub(crate) const ENCRYPTED_REFUSAL: &str = "encrypted package unsupported";

/// Whether a reader error is [`ENCRYPTED_REFUSAL`] rather than a parse failure.
pub fn is_encrypted_refusal(err: &str) -> bool {
    err.starts_with(ENCRYPTED_REFUSAL)
}

/// Upper bound on a package's entry count.
///
/// The number is a memory guard, not a property of the format: the largest
/// staged pack declares 3,499 entries, and this sits ~285x above it. Exceeding
/// it means the count word is damaged.
pub(crate) const ENTRY_LIMIT: usize = 1_000_000;

/// The revision whose trailer *is* an Angelica package trailer.
///
/// `configs.pck` and `shaders.pck` carry the same 280-byte tail as the retail
/// package's oldest revision: a 272-byte `FILEHEADER`, then the count, then the
/// version word. That is why the Angelica reader can share this table rather
/// than restate it -- the layout is not similar, it is identical, and the two
/// shipped archives' `0x00020002` word is a retail revision number.
pub(crate) fn angelica_revision(version: u32) -> Option<Revision> {
    match version {
        // `0x00020001` shares the `0x00020002` layout byte for byte, so it is
        // read on the word the archive states rather than refused for the one
        // it does not. `0x00020003` is a genuinely different, longer trailer
        // (280-byte header, 288-byte records, 8-byte offsets) and is read from
        // its own row -- 1.8.4's `configs.pck` states it and holds 1,057
        // entries that the 0x00020002 row would misparse.
        0x0002_0001 | 0x0002_0002 => REVISIONS.iter().find(|r| r.version == 0x0002_0002).copied(),
        0x0002_0003 => REVISIONS.iter().find(|r| r.version == 0x0002_0003).copied(),
        _ => None,
    }
}

/// Validate the 12-byte prefix every package starts with.
///
/// `whole` is the size of the package when its two halves are known, so a
/// declared size larger than the file on disk is the split it is. Without that
/// evidence the same bytes are a corrupt size word, and calling it a split would
/// send a reader looking for a file that does not exist.
fn safe_header(safe: &[u8], size: u64, whole: Option<u64>) -> Result<(), String> {
    if u32le(safe, 0)? != 0x4dca23ef || u32le(safe, 8)? != 0x56a089b7 {
        return Err("unsupported PCK safe header".into());
    }
    let end = u32le(safe, 4)? as u64;
    // A package longer than 4 GiB cannot state its length in this `DWORD`, so
    // the word is the total truncated. 1.8.4's `building.pck` is 2 GiB + 4 GiB +
    // a remainder, which leaves a declared value smaller than a package's own
    // header. The wrap is only believed once every piece is open, because the
    // pieces are what prove the number.
    let wrapped = whole.is_some_and(|whole| whole > u32::MAX as u64 && whole as u32 as u64 == end);
    if wrapped {
        return Ok(());
    }
    if end < 292 {
        return Err("bad package end offset".into());
    }
    if end > size {
        // The client reads a package split at `MAX_FILE_PACKAGE` together with
        // its continuations (`litmodels.pck` + `litmodels.pkx` in the 1.5.2
        // client is one 2.86 GB archive). With every piece open the declared
        // size is simply the package's size and is not an error.
        //
        // The size word is a `DWORD`, so a package longer than 4 GiB cannot
        // state its length: 1.8.4's `building.pck` is 2 GiB + 4 GiB + a
        // remainder, and the word is that total truncated. A wrapped match is
        // accepted for that reason, and only once the pieces are all open --
        // the check below is what proves they are the right pieces.
        if whole == Some(end) {
            return Ok(());
        }
        return Err(match whole {
            Some(whole) => format!(
                "split package: the header declares {end} bytes but the two halves hold {whole}"
            ),
            // A file of exactly `MAX_FILE_PACKAGE` is the first half of a split
            // package even when no `.pkx` sits beside it, so the missing half is
            // named rather than the size word being called corrupt.
            None if size == MAX_FILE_PACKAGE => format!(
                "split package: the header declares {end} bytes, this file holds the first \
                 {size} of them and the `.pkx` continuation is missing"
            ),
            None => format!(
                "bad package end offset: the header declares {end} bytes but this file holds {size}"
            ),
        });
    }
    Ok(())
}

/// The size the client's `MAX_FILE_PACKAGE` splits a package at.
///
/// `AFilePackage` starts a new file once a package has written this much, which
/// is why a 2.86 GB `litmodels.pck` sits on disk as a 2,147,483,392-byte
/// `litmodels.pck` (`0x7fffff00` = this constant) plus the remainder in
/// `litmodels.pkx`.
///
/// A file is *not* capped at this size: the 1.8.4 client ships a
/// `building.pkx` of 4,294,966,784 bytes, two splits' worth in one file. Piece
/// lengths therefore have to be measured, not assumed.
pub const MAX_FILE_PACKAGE: u64 = 0x7fff_ff00;

/// Where each file of one package sits in the logical byte stream.
///
/// An unsplit package is just the `.pck`. Otherwise the client appends files of
/// the same stem — `.pkx`, `.pkx1`, … — and every entry offset is measured from
/// the start of the *first* file, so a package is one continuous address space
/// spread over the pieces.
///
/// The pieces are not all [`MAX_FILE_PACKAGE`] long. The 1.8.4 client ships
/// `building.pck` (2,147,483,392) + `building.pkx` (4,294,966,784) +
/// `building.pkx1` (1,733,966,474), and that middle file is two splits' worth in
/// one file, which is why the lengths are recorded as they are on disk rather
/// than derived from a split count.
#[derive(Clone)]
pub struct Layout {
    /// `(logical start, length)` for each file, in order. Empty only for a
    /// layout built by hand.
    pieces: Vec<(u64, u64)>,
    /// Bytes of the `.pck` (`pieces[0]`), or the whole file when unsplit.
    pub part0: u64,
    /// Total bytes the package spans.
    span: u64,
}

impl Layout {
    /// Every byte of one file.
    pub fn single(len: u64) -> Self {
        Self {
            pieces: vec![(0, len)],
            part0: len,
            span: len,
        }
    }

    /// A package spread over `lens`, in order.
    fn of_pieces(lens: &[u64]) -> Self {
        let mut pieces = Vec::with_capacity(lens.len());
        let mut start = 0u64;
        for len in lens {
            pieces.push((start, *len));
            start = start.saturating_add(*len);
        }
        Self {
            pieces,
            part0: lens.first().copied().unwrap_or(0),
            span: start,
        }
    }

    /// The package as a single logical byte range, so a caller can check an
    /// entry's span against one number.
    pub fn total(&self) -> u64 {
        self.span
    }

    /// The logical start of piece `index`.
    fn start_of(&self, index: usize) -> Option<u64> {
        self.pieces.get(index).map(|(start, _)| *start)
    }

    /// The logical start of the last piece, where the trailer is written.
    fn last_start(&self) -> u64 {
        self.pieces.last().map(|(start, _)| *start).unwrap_or(0)
    }

    /// How many pieces the package holds.
    fn len(&self) -> usize {
        self.pieces.len()
    }
}

impl std::fmt::Debug for Layout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Layout{:?} total={}", self.pieces, self.span)
    }
}

/// Open the `.pkx` that continues a `.pck` cut at [`MAX_FILE_PACKAGE`].
///
/// Only a `.pck` that reaches the split point can have one, so a stray `.pkx`
/// next to a small pack is never mounted.
pub fn open_continuation(path: &Path) -> Option<(File, u64)> {
    let len = fs::metadata(path).ok()?.len();
    if len != MAX_FILE_PACKAGE {
        return None;
    }
    let mut name = path.file_stem()?.to_os_string();
    name.push(".pkx");
    let pkx = path.with_file_name(name);
    let file = File::open(&pkx).ok()?;
    let len = file.metadata().ok()?.len();
    Some((file, len))
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
fn read_trailer(f: &File, end: u64) -> Result<(Revision, usize, Vec<u8>), String> {
    let version = u32le(&at_shared(f, end - 4, 4)?, 0)?;
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
    let count = u32le(&at_shared(f, end - 8, 4)?, 0)? as usize;
    let header = at_shared(f, header_start, rev.header)?;
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
    let file = f.try_clone().map_err(|e| e.to_string())?;
    read_directory(&[(file, size)], &Layout::single(size), end, 0)
}

/// Read the trailer and the entry table.
///
/// `end` is the end of the logical package and `base` the position inside it
/// where the trailer-bearing half starts: zero for a single file, and
/// [`MAX_FILE_PACKAGE`] for the `.pkx` half of a split package. The trailer's
/// own offsets are measured from the start of *its* file, which is why both
/// numbers are needed.
fn read_directory(
    files: &[(File, u64)],
    layout: &Layout,
    end: u64,
    base: u64,
) -> Result<Vec<Entry>, String> {
    // The trailer is written in the last file of the package.
    let reader: &File = &files.last().ok_or("package holds no file")?.0;
    let part_end = end - base;
    let (rev, count, header) = read_trailer(reader, part_end)?;
    let flags = u32le(&header, rev.flags_at)?;
    if flags & ENCRYPTED_FLAG != 0 {
        // Name the revision and the flag word: "unsupported" without the
        // revision leaves a reader unable to tell an enciphered package from a
        // truncated one, and the version is the thing that would have to be
        // supported next.
        return Err(format!(
            "{ENCRYPTED_REFUSAL}: revision {:08x} sets dwFlags {flags:08x} \
             (bit 31); this build reads the directory only in the clear",
            rev.version
        ));
    }
    // Measured maxima, largest staged pack first: `gfx.pck` 3,499 entries,
    // `grasses.pck` 1,453, `textures.pck` 943, `trees.pck` 326, `configs.pck`
    // 58. The limit is two orders of magnitude above the largest of those and
    // exists to stop a damaged count from reserving memory, not to police what
    // a newer client may ship -- so it is a bound, not a format rule.
    if count > ENTRY_LIMIT {
        return Err(format!("entry count {count} exceeds limit {ENTRY_LIMIT}"));
    }
    // The client unmasks the entry offset in place (`dwEntryOffset ^=
    // AFPCK_MASKDWORD`) and seeks there; the trailer follows the table, so a
    // table that starts at or past it is not in this package.
    //
    // The word is 32 bits, so it cannot address a package longer than 4 GiB.
    // A split package therefore has to mean one of two things by it, and the
    // shipped clients differ:
    //
    //   * package-absolute, as every entry's `dwOffset` in this revision is:
    //     the table's position counted from the start of the first file. This
    //     is the only reading that works for a package under 4 GiB, so it is
    //     tried first.
    //   * piece-local, counted from the start of the file holding the trailer.
    //     A package of 1.5.2's `litmodels` is 2,859,188,412 bytes, and its
    //     `.pkx` holds the last 711,705,020 of them, so a package-absolute
    //     32-bit word cannot name its table at all. That client must be writing
    //     the offset from the start of the piece.
    //
    // Reading only the package-absolute way rejected that archive as "entry
    // table overlaps footer" -- a 32-bit position was compared against the
    // piece's length -- which is why 1.5.2 resolved none of its 22,981 scenery
    // models. Both readings are attempted, in that order, and the first one
    // that yields a well-formed table wins.
    let pos = (u32le(&header, 8)? ^ MASK) as u64;
    let table_end = part_end - (rev.header + 8) as u64;
    let mut tried = Vec::new();
    for (label, local) in [
        ("package-absolute", pos.checked_sub(base)),
        ("piece-local", Some(pos)),
    ] {
        let Some(local) = local.filter(|local| *local <= table_end) else {
            continue;
        };
        // The entry table may also straddle the split, so it is walked through
        // the two-part reader rather than one file.
        let table = read_package_range(files, layout, base + local, table_end - local)?;
        match read_entry_table(&table, count, &rev, end, layout.total()) {
            Ok(entries) => return Ok(entries),
            Err(e) => tried.push(format!("{label} {local}: {e}")),
        }
    }
    // Nothing parsed. The masked offset in the trailer's header is then either
    // a position past the end or the client's "no table here" sentinel, and
    // "overlaps footer" alone cannot say which; the size word and the offset
    // word are the only evidence there is.
    Err(format!(
        "entry table overlaps footer: the trailer declares its table at {pos} but only \
         {table_end} bytes of this {} (len {}) precede the footer (declared end {end}, base \
         {base}, count {count}, flags {:#010x}); {}",
        if base == 0 { "file" } else { "piece" },
        part_end,
        u32le(&header, rev.flags_at).unwrap_or(0),
        if tried.is_empty() {
            "neither reading fits inside the footer".to_string()
        } else {
            tried.join("; ")
        }
    ))
}

/// A whole package, which is one file unless it was split at
/// [`MAX_FILE_PACKAGE`].
///
/// Once a package reaches [`MAX_FILE_PACKAGE`] the client starts the next file
/// of the same stem with the `.pkx` extension and keeps writing, and an entry's
/// `dwOffset` is measured from the start of the *first* file, so it runs past
/// the split point instead of restarting at zero. The 1.5.2 client ships
/// `litmodels.pck` exactly that way — a 2,147,483,392-byte `.pck` holding the
/// first half of a 2,859,188,412-byte package, with the rest in
/// `litmodels.pkx` — and 1.8.4's `building` goes on to a `.pkx1`.
///
/// Reading one half alone rejected the archive as corrupt and left every
/// scenery model in the map unavailable, so the pair is opened and read as the
/// single package the client sees.
pub struct Package {
    path: PathBuf,
    first: File,
    /// Continuation files after the `.pck`, in order. Each is kept with its
    /// length so a reader can find which one holds a position without asking
    /// the filesystem again.
    continuations: Vec<(File, u64)>,
    layout: Layout,
}

impl Package {
    /// Open `path`, picking up the `.pkx` continuation when `path` is the first
    /// half of a split package.
    pub fn open(path: &Path) -> Result<Self, String> {
        let first = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let size = first.metadata().map_err(|e| e.to_string())?.len();
        let mut lengths = vec![size];
        let mut continuations = Vec::new();
        if size == MAX_FILE_PACKAGE {
            // `.pkx`, then `.pkx1`, `.pkx2` ... until one is missing. Every
            // piece's own length is recorded: they are not all a split wide.
            // The 1.8.4 client ships `building.pck` (2,147,483,392) +
            // `building.pkx` (4,294,966,784) + `building.pkx1` (1,733,966,474),
            // and reading only where a split count says the files begin leaves
            // every entry past the first file unreadable.
            let stem = path.file_stem().ok_or("package has no stem")?;
            let mut index = 0usize;
            loop {
                let mut name = stem.to_os_string();
                name.push(".pkx");
                if index > 0 {
                    name.push(index.to_string());
                }
                let next = path.with_file_name(name);
                let Ok(file) = File::open(&next) else { break };
                let len = file.metadata().map_err(|e| e.to_string())?.len();
                lengths.push(len);
                continuations.push((file, len));
                index += 1;
            }
        }
        Ok(Self {
            path: path.to_path_buf(),
            first,
            continuations,
            layout: Layout::of_pieces(&lengths),
        })
    }

    /// Whether the package is stored as a `.pck` plus continuations.
    pub fn is_split(&self) -> bool {
        !self.continuations.is_empty()
    }

    /// How many continuation files complete this package.
    pub fn continuation_count(&self) -> usize {
        self.continuations.len()
    }

    /// The first continuation (`.pkx`) that completes this package, when split.
    pub fn continuation(&self) -> Option<PathBuf> {
        self.continuation_path(1)
    }

    /// Continuation file `n`, 1-based, whether or not it exists on disk.
    pub fn continuation_path(&self, n: usize) -> Option<PathBuf> {
        if n == 0 || n > self.continuations.len() {
            return None;
        }
        let mut name = self.path.file_stem().unwrap_or_default().to_os_string();
        name.push(".pkx");
        if n > 1 {
            name.push((n - 1).to_string());
        }
        Some(self.path.with_file_name(name))
    }

    /// Bytes of the whole logical package.
    pub fn len(&self) -> u64 {
        self.layout.total()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The first file of the package: always the one that was opened.
    pub fn first_file(&self) -> &File {
        &self.first
    }

    /// The first continuation's file, for code that reads the pieces itself.
    pub fn second_file(&self) -> Option<&File> {
        self.continuations.first().map(|(file, _)| file)
    }

    /// The continuation files in order, with their lengths.
    pub fn files(&self) -> Vec<&(File, u64)> {
        self.continuations.iter().collect()
    }

    /// The pieces' sizes, for code that reads them itself.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The ordered file list `read_package_range` needs.
    fn parts(&self) -> Vec<(File, u64)> {
        let mut parts = Vec::with_capacity(1 + self.continuations.len());
        // `try_clone` shares the file offset, but every read here is positional
        // (`read_at` / `seek_read`), so the cursor is never used.
        if let Ok(clone) = self.first.try_clone() {
            parts.push((clone, self.layout.part0));
        }
        for (file, len) in &self.continuations {
            if let Ok(clone) = file.try_clone() {
                parts.push((clone, *len));
            }
        }
        parts
    }

    /// The package's own entry directory.
    pub fn directory(&mut self) -> Result<Vec<Entry>, String> {
        let safe = at(&mut self.first, 0, 12)?;
        // A split is only the explanation when the two halves add up to what the
        // header declares; safe_header says so, and read_directory re-checks the
        // table's own offsets.
        safe_header(
            &safe,
            self.layout.part0,
            if self.is_split() {
                Some(self.len())
            } else {
                None
            },
        )?;
        let parts = self.parts();
        if self.is_split() {
            // The trailer lives in the last file, whose own logical base is
            // where its length begins counting.
            return read_directory(&parts, &self.layout, self.len(), self.last_part_base());
        }
        let end = package_end(&mut self.first)?;
        read_directory(&parts, &self.layout, end, 0)
    }

    /// One entry's bytes, spanning the split when the payload does.
    pub fn read(&self, e: &Entry) -> Result<Vec<u8>, String> {
        let stored = read_package_range(
            &self.parts(),
            &self.layout,
            e.offset as u64,
            e.compressed as u64,
        )?;
        if e.compressed < e.size {
            inflate(&stored, e.size as usize)
        } else {
            Ok(stored)
        }
    }

    /// Read a batch of entries, across however many files the package spans.
    pub fn read_many(&self, entries: &[&Entry]) -> Result<Vec<Vec<u8>>, String> {
        read_entries_in(&self.parts(), &self.layout, entries).map_err(|error| error.to_string())
    }

    /// The pieces in order with their logical lengths, for a caller that reads
    /// batches in its own threads.
    pub fn part_list(&self) -> Vec<(File, u64)> {
        self.parts()
    }

    /// The logical position where the last file of a split package begins.
    fn last_part_base(&self) -> u64 {
        self.layout.last_start()
    }
}

/// Read `len` bytes starting at `pos` inside the logical package, which spans
/// `first` and then `second` when the package is split.
fn read_package_range(
    files: &[(File, u64)],
    layout: &Layout,
    pos: u64,
    len: u64,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(len as usize);
    let mut left = len;
    let mut cursor = pos;
    while left > 0 {
        // Which opened file holds `cursor`, and how far into it. The starts come
        // from the layout, so a piece that is wider than one split is just a
        // piece.
        let index = (0..layout.len())
            .rev()
            .find(|&i| layout.start_of(i).is_some_and(|start| cursor >= start))
            .unwrap_or(0);
        let start = layout.start_of(index).unwrap_or(0);
        let local = cursor - start;
        let file_len = files.get(index).map(|(_, len)| *len).unwrap_or(0);
        let take = left.min(file_len.saturating_sub(local));
        if take == 0 {
            return Err(match index {
                0 => "split package range is empty".to_string(),
                n => format!(
                    "entry runs past the end of continuation file {n} of {}",
                    layout.len() - 1
                ),
            });
        }
        // Every file is read positionally, so reading one never disturbs a
        // cursor another file's caller may hold.
        let (file, _) = files.get(index).ok_or_else(|| {
            format!(
                "entry continues into a missing continuation file {index} of {}",
                layout.len() - 1
            )
        })?;
        out.extend_from_slice(&at_shared(file, local, take as usize)?);
        cursor += take;
        left -= take;
    }
    Ok(out)
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
        // `0x00020003`. It is kept 64 bits wide here because a package can be
        // longer than 4 GiB and its far entries are addressed by their real
        // offset: 1.8.4's `building.pck` is 2 GiB + 4 GiB + 1.7 GiB, and
        // narrowing the word would turn an entry past the fourth split into a
        // near one and read somebody else's bytes.
        let offset = if rev.offset_width == 8 {
            u64::from_le_bytes(data[rev.offset_at..rev.offset_at + 8].try_into().unwrap())
        } else {
            u32le(data, rev.offset_at)? as u64
        };
        let file_size = u32le(data, rev.size_at)?;
        let compressed = u32le(data, rev.compressed_at)?;
        if offset + compressed as u64 > end
            || offset + compressed as u64 > size
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
    // The client seeks to the table and reads exactly `count` records; it never
    // looks at what follows. Bytes left over after the last record therefore
    // mean the framing was misread — a wrong count, length or encryption flag —
    // and the entries that came out are not necessarily the archive's. The
    // shipped packs end exactly on the last record, so this costs no coverage
    // and turns a silently wrong directory into a refusal.
    if cursor != table.len() {
        return Err(format!(
            "{} trailing bytes after the last of {count} entry records",
            table.len() - cursor
        ));
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
    read_entries_parts(f, None, &Layout::single(u64::MAX), entries)
}

/// Read a batch of entries out of a package that may be split in two.
///
/// Everything is positioned I/O, so the same batch can span both halves without
/// disturbing any cursor; an entry that starts in `first` and ends in `second`
/// is simply read twice.
///
/// A package split into *more* than two files cannot be read through this
/// signature: `second` is one file, and the third piece has nowhere to go.
/// `read_entries_in` takes the whole list and is what a `Package` uses.
pub fn read_entries_parts(
    first: &File,
    second: Option<&File>,
    layout: &Layout,
    entries: &[&Entry],
) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    // `layout.part0` is the `.pck`, and the caller passes the single
    // continuation it claims to have; the pair must still describe the whole
    // package for the piece starts in `layout` to line up with this list.
    let mut files: Vec<(File, u64)> = vec![(first.try_clone()?, layout.part0)];
    if let Some(second) = second {
        let len = layout.total().saturating_sub(layout.part0);
        files.push((second.try_clone()?, len));
    }
    read_entries_in(&files, layout, entries)
}

/// Read a batch of entries out of a package's own files, however many there are.
///
/// `files` is the package in order with each piece's logical length, the same
/// list `read_package_range` takes. Every read is positioned, so one batch can
/// cross any number of piece boundaries.
pub fn read_entries_in(
    first_and_rest: &[(File, u64)],
    layout: &Layout,
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
            // A split package lives in several files and this queue is opened
            // on one, so a split never takes the DirectStorage path; the
            // positioned read below handles every piece.
            if first_and_rest.len() > 1 {
                continue;
            }
            ranges.push((entry.offset as u64, bytes.as_mut_slice()));
            range_indices.push(index);
            if ranges.len() == BATCH_CAPACITY {
                if direct_storage::read_ranges(&first_and_rest[0].0, &mut ranges) {
                    for &index in &range_indices {
                        direct_storage_done[index] = true;
                    }
                }
                ranges.clear();
                range_indices.clear();
            }
        }
        if !ranges.is_empty() && direct_storage::read_ranges(&first_and_rest[0].0, &mut ranges) {
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
        // Walk the pieces in order, so an entry that begins in one and ends in
        // another is read from each in turn -- with any number of continuations,
        // not just one.
        while read < bytes.len() {
            let cursor = entry.offset as u64 + read as u64;
            // The piece whose logical range holds this position. Starts come from
            // the layout, so a file wider than one split needs no special case.
            let piece = (0..layout.len())
                .rev()
                .find(|&i| {
                    layout.start_of(i).is_some_and(|start| {
                        cursor >= start && cursor < start + first_and_rest[i].1
                    })
                })
                .ok_or_else(|| {
                    format!(
                        "PCK entry {} runs past the last piece of the package",
                        entry.name
                    )
                })?;
            let start = layout.start_of(piece).unwrap_or(0);
            let (file, file_len) = &first_and_rest[piece];
            let off = cursor - start;
            let remaining = start + *file_len - cursor;
            let room = remaining.min((bytes.len() - read) as u64) as usize;
            #[cfg(windows)]
            let n =
                std::os::windows::fs::FileExt::seek_read(file, &mut bytes[read..read + room], off)?;
            #[cfg(unix)]
            let n = std::os::unix::fs::FileExt::read_at(file, &mut bytes[read..read + room], off)?;
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
    SKIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    eprintln!(
        "SKIPPED: no {what} on this machine; this test proves nothing about the shipped data \
         (set {CORPUS_ENV}=require to make this a failure)"
    );
}

/// Report a *member* the staged archive does not carry, and skip.
///
/// The staged `gfx.pck` on a given machine is whatever build was downloaded, and
/// builds differ: a 1.x pack has no `gfx/场景/新种族地图/` tree at all, so four
/// tests that pass on the retail pack panicked with "is not in gfx.pck" on this
/// one. That is a property of the staged file, not a regression in the reader, so
/// it is a counted skip — the same rule as a missing archive, only narrower. It
/// is deliberately not a silent `return`: the count is printed at the end of the
/// run so a suite cannot claim coverage it did not have.
#[cfg(test)]
pub fn corpus_member_absent(name: &str) {
    SKIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if corpus_required() {
        panic!("{CORPUS_ENV}=require, but the staged archive has no {name}");
    }
    eprintln!(
        "SKIPPED: the staged archive has no {name}; this build predates or postdates that record \
         (set {CORPUS_ENV}=require to make this a failure)"
    );
}

/// Report a *file* of a staged client that this machine does not carry, and
/// skip.
///
/// This is [`corpus_absent`] for a path rather than a bare name, and it exists
/// because the tests that lean on a staged client used to guard themselves with
/// a bare `return`:
///
/// ```ignore
/// if !element.join("models.pck").exists() {
///     return;
/// }
/// ```
///
/// A `return` from a `#[test]` *passes* it. On a machine with no staged client
/// those tests therefore reported success while never opening a single archive
/// -- the suite claimed coverage of the shipped data that it did not have, which
/// is the one thing a corpus test must not do. The count is printed at the end
/// of the run, so "211 passed" cannot quietly mean "180 passed and 31 looked at
/// nothing".
#[cfg(test)]
pub fn corpus_file_absent(path: &std::path::Path) {
    corpus_file_absent_with(corpus_required(), path);
}

/// [`corpus_file_absent`] with the decision already made.
#[cfg(test)]
pub fn corpus_file_absent_with(required: bool, path: &std::path::Path) {
    if required {
        panic!(
            "{CORPUS_ENV}=require, but this machine has no {}",
            path.display()
        );
    }
    SKIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    eprintln!(
        "SKIPPED: no {} on this machine; this test proves nothing about the shipped data \
         (set {CORPUS_ENV}=require to make this a failure)",
        path.display()
    );
}

/// How many corpus tests skipped for a missing archive so far.
///
/// A skip is not a pass, and a suite of 282 passes reads as full coverage even
/// when a dozen of them never looked at the data they exist to check. The number
/// is counted here and reprinted by the end-of-run hook below, so a run says
/// "282 passed, 4 skipped" instead of pretending the two are the same. The
/// harness prints it from `skipped_corpus_tests()`; CI sets `{CORPUS_ENV}=require`
/// and fails instead.
#[cfg(test)]
pub static SKIPPED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Corpus tests that skipped in this process.
#[cfg(test)]
pub fn skipped_corpus_tests() -> usize {
    SKIPPED.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An enciphered package must be refused by *name*, not as corruption.
    ///
    /// A census that cannot tell "feature this build lacks" from "archive is
    /// damaged" reports a shipped pack as broken. `is_encrypted_refusal` is the
    /// predicate the survey tools use to count it as a skip instead, so it has
    /// to be true for the refusal this reader actually produces and false for
    /// every other one.
    #[test]
    fn the_encrypted_refusal_is_named_and_recognisable() {
        assert!(is_encrypted_refusal(ENCRYPTED_REFUSAL));
        assert!(is_encrypted_refusal(&format!(
            "{ENCRYPTED_REFUSAL}: revision 00020002 sets dwFlags 80000000 (bit 31)"
        )));
        // The neighbouring refusals must not read as an encrypted package.
        for other in [
            "unsupported PCK version 00020009",
            "unsupported PCK guard keys",
            "entry count exceeds limit",
            "not an Angelica file package (description tag missing)",
        ] {
            assert!(
                !is_encrypted_refusal(other),
                "{other:?} must not be mistaken for an encrypted package"
            );
        }
        assert_eq!(ENCRYPTED_FLAG, 1 << 31);
    }

    /// The Angelica readers and the retail reader must agree on the trailer.
    ///
    /// `configs.pck` and `shaders.pck` carry the retail package's oldest
    /// revision, and this asserts it rather than restating it: if a later change
    /// moves the offsets in `REVISIONS`, `angelica_pck` moves with them instead
    /// of silently keeping a second, divergent copy of the layout.
    #[test]
    fn the_angelica_trailer_is_the_retail_revisions_trailer() {
        let v1 = angelica_revision(0x0002_0001).expect("0x00020001 is an Angelica trailer");
        let v2 = angelica_revision(0x0002_0002).expect("0x00020002 is an Angelica trailer");
        for rev in [v1, v2] {
            let retail = REVISIONS
                .iter()
                .find(|r| r.version == 0x0002_0002)
                .expect("the retail table carries 0x00020002");
            assert_eq!(rev.header, retail.header);
            assert_eq!(rev.guard_byte1, retail.guard_byte1);
            // 272-byte header + count + version word is the 280-byte tail the
            // Angelica reader anchors its walk on.
            assert_eq!(rev.header + 8, 280);
            assert_eq!(rev.guard_byte1, 268);
        }
        // 0x00020003 is a real, different revision: 1.8.4's `configs.pck`
        // states it, and it resolves to its own row rather than the 0x00020002
        // one, whose 276-byte records cannot represent its 288-byte entries.
        let v3 = angelica_revision(0x0002_0003).expect("0x00020003 is an Angelica trailer");
        assert_eq!(v3.entry, 288);
        assert_eq!(v3.offset_width, 8);
        assert_eq!(v3.header + 8, 288);
        // An unknown word is still refused.
        assert!(angelica_revision(0x0000_0000).is_none());
        assert!(angelica_revision(0x0002_0004).is_none());
    }

    /// There are two shipped guard pairs, and conflating them refused a whole
    /// client. Both are pinned against the archives they were measured on:
    /// 1.5.2's `configs.pck`/`grasses.pck` carry `a508bdc4`/`853567c8`, 1.8.4's
    /// `configs.pck` carries `fdfdfeee`/`f00dbeef`.
    #[test]
    fn both_shipped_guard_pairs_are_distinct_and_pinned() {
        assert_eq!(GUARD_0, 0xfdfdfeee);
        assert_eq!(GUARD_1, 0xf00dbeef);
        assert_eq!(ANGELICA_GUARD_0, 0xa508bdc4);
        assert_eq!(ANGELICA_GUARD_1, 0x853567c8);
        assert_ne!((GUARD_0, GUARD_1), (ANGELICA_GUARD_0, ANGELICA_GUARD_1));
        let v2 = REVISIONS.iter().find(|r| r.version == 0x0002_0002).unwrap();
        assert_eq!(v2.guard_byte1, 268);
    }

    /// A missing staged-client file must count as a skip, not as a pass.
    ///
    /// The tests this helper replaced guarded themselves with `return;`, which
    /// libtest reads as success. That is the whole defect: 28 tests across eight
    /// modules reported "ok" on a machine with no client staged.
    #[test]
    fn a_missing_client_file_is_counted_not_silently_passed() {
        let before = SKIPPED.load(std::sync::atomic::Ordering::Relaxed);
        // Not a path this repository ships, so it is absent on every machine.
        let absent = std::path::Path::new("google_drive/pw142/element/models.pck");
        corpus_file_absent_with(false, absent);
        assert_eq!(
            SKIPPED.load(std::sync::atomic::Ordering::Relaxed),
            before + 1,
            "the skip must be counted, or a run cannot tell it from a pass"
        );
    }

    /// And with the gate on it must fail, not skip: CI sets `PWE_CORPUS=require`
    /// precisely so a machine without the corpus cannot report success.
    #[test]
    #[should_panic(expected = "PWE_CORPUS=require")]
    fn a_missing_client_file_fails_when_the_corpus_is_required() {
        corpus_file_absent_with(true, std::path::Path::new("google_drive/pw142/element"));
    }

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
                offset: first_offset as u64,
                size: first.len() as u32,
                compressed: first.len() as u32,
            },
            Entry {
                name: "second".into(),
                offset: second_offset as u64,
                size: second.len() as u32,
                compressed: packed_second.len() as u32,
            },
            Entry {
                name: "third".into(),
                offset: third_offset as u64,
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

    /// `0x00020003` widens `dwOffset` to 64 bits, but `Entry` and the client's
    /// own `FILEENTRY` hold 32. Truncating would fold a far entry onto a near
    /// one and read the wrong bytes — a silent corruption of a perfectly good
    /// payload. An archive that can hold such an offset is past 4 GiB, which no
    /// shipped pack reaches, so the entry is named and refused instead.
    #[test]
    fn an_entry_offset_past_four_gibibytes_is_kept_wide_not_truncated() {
        let mut bytes = package(0x0002_0003, &[("far.bin", b"payload")], false);
        // The entry table starts where the trailer's `dwEntryOffset` says, after
        // the mask is removed; the record itself begins 8 bytes later (the
        // masked length and check word).
        let header_at = bytes.len() - 280 - 8;
        let table =
            u32::from_le_bytes(bytes[header_at + 8..header_at + 12].try_into().unwrap()) ^ MASK;
        let record_start = table as usize + 8;
        // Rewrite the record's 64-bit dwOffset to 2^32, which truncates to 0 --
        // the *start of the package* -- if the reader is careless. The 1.8.4
        // client ships a 8.2 GB `building.pck`, so an offset this large is not
        // hypothetical: it is what its far entries carry. The offset must be
        // kept, and the run past the package's end is what refuses it.
        bytes[record_start + 264..record_start + 272].copy_from_slice(&(1u64 << 32).to_le_bytes());
        let path = write_temp(&bytes, "far");
        let mut file = std::fs::File::open(&path).unwrap();
        let error = directory(&mut file).unwrap_err();
        drop(file);
        std::fs::remove_file(path).unwrap();
        assert!(
            error.contains("invalid entry bounds"),
            "the far offset must be bounds-checked, not truncated to the start of the \
             package: {error}"
        );
    }

    /// The client reads exactly `count` records and never looks at what
    /// follows, so leftover bytes mean the framing was misread (a wrong count,
    /// length or encryption flag) and the entries that came out are not
    /// necessarily the archive's. Every shipped pack ends exactly on its last
    /// record, so refusing the mismatch costs no coverage.
    #[test]
    fn trailing_bytes_after_the_last_entry_record_are_a_framing_error() {
        let good = package(0x0002_0003, &[("a.txt", b"one")], false);
        let path = write_temp(&good, "exact");
        let mut file = std::fs::File::open(&path).unwrap();
        let entries = directory(&mut file).unwrap();
        assert_eq!(entries.len(), 1, "the exact package must still read");
        drop(file);
        std::fs::remove_file(&path).unwrap();

        // Declare one more record than exists: the walk runs off the end of the
        // table and has to say so rather than returning a short list.
        let mut fewer = package(0x0002_0003, &[("a.txt", b"one")], false);
        let count_at = fewer.len() - 8;
        fewer[count_at..count_at + 4].copy_from_slice(&2u32.to_le_bytes());
        let path = write_temp(&fewer, "shorttable");
        let mut file = std::fs::File::open(&path).unwrap();
        let error = directory(&mut file).unwrap_err();
        drop(file);
        std::fs::remove_file(&path).unwrap();
        assert!(
            error.contains("entry table") || error.contains("trailing"),
            "a count past the table must be refused: {error}"
        );
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

    /// A declared size larger than the file is a split when the two halves add
    /// up to it, and corruption when they do not.
    ///
    /// "Corrupt archive" would send a reader after damage that is not there: the
    /// archive is whole, it is just stored in two files.
    #[test]
    fn a_split_package_must_have_halves_that_add_up_to_the_declared_size() {
        let mut bytes = package(0x0002_0003, &[("a.txt", b"x")], false);
        let declared = bytes.len() as u64 + 4096;
        let size = bytes.len() as u64;
        bytes[4..8].copy_from_slice(&(declared as u32).to_le_bytes());
        safe_header(&bytes[..12], size, Some(declared))
            .expect("the two halves add up to the declared size");
        let error = safe_header(&bytes[..12], size, Some(declared + 1)).unwrap_err();
        assert!(error.contains("split package"), "unexpected error: {error}");
        assert!(error.contains("two halves"), "unexpected error: {error}");
    }

    /// Build the retail split: a `.pck` of exactly `MAX_FILE_PACKAGE` holding
    /// the start of the package, and a `.pkx` holding everything after it.
    ///
    /// The trailer and the entry table live in the second half, which is what
    /// made a half-read fatal rather than partial. `straddler` is placed so that
    /// it begins just before the split point and ends just after it, so reading
    /// it needs both files.
    fn split_package(tag: &str, straddler: &[u8]) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::io::{Seek, SeekFrom};
        let dir = std::env::temp_dir().join(format!(
            "pw-pck-split-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pck = dir.join("models.pck");
        let pkx = dir.join("models.pkx");
        let split = MAX_FILE_PACKAGE;
        // Offsets are measured from the start of the first file, so an entry
        // near the split point is a package-relative number past `split - 8`.
        // Eight bytes of the payload sit in the `.pck`, the rest in the `.pkx`.
        let straddle_at = split - 8;
        let mut head = Vec::new();
        head.extend_from_slice(&0x4dca23efu32.to_le_bytes());
        head.extend_from_slice(&0u32.to_le_bytes()); // declared size, patched below
        head.extend_from_slice(&0x56a089b7u32.to_le_bytes());
        let mut first = std::fs::File::create(&pck).unwrap();
        first.write_all(&head).unwrap();
        first.seek(SeekFrom::Start(straddle_at)).unwrap();
        first.write_all(straddler).unwrap();
        first.set_len(split).unwrap();
        drop(first);

        // Second half: the straddler's tail, then the table and the trailer.
        let mut tail = Vec::new();
        tail.extend_from_slice(&straddler[(split - straddle_at) as usize..]);
        let table_at = tail.len();
        let mut record = vec![0u8; 288];
        let name = b"litmodels/world/a.bmd";
        record[..name.len()].copy_from_slice(name);
        record[264..272].copy_from_slice(&straddle_at.to_le_bytes());
        record[272..276].copy_from_slice(&(straddler.len() as u32).to_le_bytes());
        record[276..280].copy_from_slice(&(straddler.len() as u32).to_le_bytes());
        let masked = (record.len() as u32) ^ MASK;
        tail.extend_from_slice(&masked.to_le_bytes());
        tail.extend_from_slice(&(masked ^ CHECK).to_le_bytes());
        tail.extend_from_slice(&record);
        let mut header = vec![0u8; 280];
        header[0..4].copy_from_slice(&0xfdfdfeeeu32.to_le_bytes());
        header[4..8].copy_from_slice(&0x0002_0003u32.to_le_bytes());
        header[8..12].copy_from_slice(&((table_at as u32) ^ MASK).to_le_bytes());
        let tag_bytes = b"Angelica File Package, Perfect World Co. Ltd.";
        header[20..20 + tag_bytes.len()].copy_from_slice(tag_bytes);
        header[272..276].copy_from_slice(&0xf00dbeefu32.to_le_bytes());
        tail.extend_from_slice(&header);
        tail.extend_from_slice(&1u32.to_le_bytes());
        tail.extend_from_slice(&0x0002_0003u32.to_le_bytes());
        std::fs::write(&pkx, &tail).unwrap();

        // The header declares the size of the whole package.
        let declared = (split + tail.len() as u64) as u32;
        let mut file = std::fs::OpenOptions::new().write(true).open(&pck).unwrap();
        file.seek(SeekFrom::Start(4)).unwrap();
        file.write_all(&declared.to_le_bytes()).unwrap();
        drop(file);
        (dir, pck)
    }

    /// A package larger than two `MAX_FILE_PACKAGE` pieces continues in
    /// `.pkx1`, `.pkx2` ... and must be read through all of them.
    ///
    /// The 1.8.4 client ships `building.pck` this way: 2,147,483,392 bytes of
    /// `.pck`, a `.pkx` of 4,294,966,784, and a `.pkx1`. Opening only the first
    /// two left every entry past the second split unreadable, which surfaces as
    /// an archive whose models simply are not there.
    #[test]
    fn a_package_spanning_three_files_reads_through_the_last_one() {
        use std::io::{Seek, SeekFrom};
        let dir = std::env::temp_dir().join(format!(
            "pw-pck-pkx1-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pck = dir.join("building.pck");
        let pkx = dir.join("building.pkx");
        let pkx1 = dir.join("building.pkx1");
        let split = MAX_FILE_PACKAGE;
        // A payload that starts just before the *second* split, so it needs the
        // `.pkx1` that only a three-file reader opens.
        let straddle_at = 2 * split - 8;
        let payload = b"ABCDEFGHpayload-that-lives-past-the-second-split-point";

        let mut head = Vec::new();
        head.extend_from_slice(&0x4dca23efu32.to_le_bytes());
        head.extend_from_slice(&0u32.to_le_bytes());
        head.extend_from_slice(&0x56a089b7u32.to_le_bytes());
        let mut first = std::fs::File::create(&pck).unwrap();
        first.write_all(&head).unwrap();
        first.set_len(split).unwrap();
        drop(first);

        // The payload starts eight bytes before the *second* split, so its
        // first eight bytes close out the `.pkx` and the rest lives in
        // `.pkx1`. A reader that stops after the `.pkx` returns zeros here.
        //
        // The piece is 2 GiB wide but holds eight meaningful bytes, and it must
        // be *sparse*: materialising 2 GiB of zeros (let alone the 4 GiB path
        // through `write_all` of a buffer) fills the disk the tests run on.
        let mut middle = std::fs::File::create(&pkx).unwrap();
        middle.set_len(split).unwrap();
        middle.seek(SeekFrom::Start(split - 8)).unwrap();
        middle.write_all(&payload[..8]).unwrap();
        drop(middle);

        let mut tail = Vec::new();
        tail.extend_from_slice(&payload[8..]);
        let table_at = tail.len();
        let mut record = vec![0u8; 288];
        let name = b"litmodels/building/a.bmd";
        record[..name.len()].copy_from_slice(name);
        record[264..272].copy_from_slice(&straddle_at.to_le_bytes());
        record[272..276].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        record[276..280].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        let masked = (record.len() as u32) ^ MASK;
        tail.extend_from_slice(&masked.to_le_bytes());
        tail.extend_from_slice(&(masked ^ CHECK).to_le_bytes());
        tail.extend_from_slice(&record);
        let mut header = vec![0u8; 280];
        header[0..4].copy_from_slice(&0xfdfdfeeeu32.to_le_bytes());
        header[4..8].copy_from_slice(&0x0002_0003u32.to_le_bytes());
        header[8..12].copy_from_slice(&((table_at as u32) ^ MASK).to_le_bytes());
        let tag = b"Angelica File Package, Perfect World Co. Ltd.";
        header[20..20 + tag.len()].copy_from_slice(tag);
        header[272..276].copy_from_slice(&0xf00dbeefu32.to_le_bytes());
        tail.extend_from_slice(&header);
        tail.extend_from_slice(&1u32.to_le_bytes());
        tail.extend_from_slice(&0x0002_0003u32.to_le_bytes());
        std::fs::write(&pkx1, &tail).unwrap();

        let declared = (2 * split + tail.len() as u64) as u32;
        let mut file = std::fs::OpenOptions::new().write(true).open(&pck).unwrap();
        file.seek(SeekFrom::Start(4)).unwrap();
        file.write_all(&declared.to_le_bytes()).unwrap();
        drop(file);

        let mut package = Package::open(&pck).unwrap();
        assert_eq!(
            package.continuation_count(),
            2,
            "both .pkx and .pkx1 belong to this package"
        );
        assert_eq!(package.len(), 2 * split + tail.len() as u64);
        let entries = package.directory().unwrap();
        assert_eq!(entries.len(), 1);
        let data = package.read(&entries[0]).unwrap();
        assert_eq!(
            data, payload,
            "the payload did not come back from the third file"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The 1.8.4 shipment, in which the **middle** file is two splits wide.
    ///
    /// `building.pck` is 2,147,483,392 bytes, `building.pkx` is 4,294,966,784
    /// (exactly two `MAX_FILE_PACKAGE`) and `building.pkx1` is 1,733,966,474.
    /// Every continuation therefore starts two splits after the previous one,
    /// not one, and a reader that places pieces at `n * MAX_FILE_PACKAGE` reads
    /// the third file's bytes as the second's -- or, past the fourth split,
    /// refuses the entry entirely. This fixture puts its payload inside the
    /// third file only, so a reader that misplaces the pieces cannot find it.
    #[test]
    fn a_continuation_that_is_two_splits_wide_still_reads_from_the_right_file() {
        use std::io::{Seek, SeekFrom};
        let dir = std::env::temp_dir().join(format!(
            "pw-pck-widecont-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pck = dir.join("building.pck");
        let pkx = dir.join("building.pkx");
        let pkx1 = dir.join("building.pkx1");
        let split = MAX_FILE_PACKAGE as usize;
        let wide = 2 * split;
        let payload = b"payload-stored-in-the-fourth-split";

        let mut head = Vec::new();
        head.extend_from_slice(&0x4dca23efu32.to_le_bytes());
        head.extend_from_slice(&0u32.to_le_bytes());
        head.extend_from_slice(&0x56a089b7u32.to_le_bytes());
        let mut first = std::fs::File::create(&pck).unwrap();
        first.write_all(&head).unwrap();
        first.set_len(split as u64).unwrap();
        drop(first);

        // Sparse, so two splits of doorstop cost nothing.
        let middle = std::fs::File::create(&pkx).unwrap();
        middle.set_len(wide as u64).unwrap();
        drop(middle);

        // The payload starts at the third split, four bytes in: wholly inside
        // `.pkx1`, whose logical start is three splits because the `.pkx`
        // before it is two wide.
        let payload_at = 3 * split + 4;
        let mut tail = vec![0u8; 4];
        tail.extend_from_slice(payload);
        let table_at = tail.len();
        let mut record = vec![0u8; 288];
        let name = b"litmodels/building/b.bmd";
        record[..name.len()].copy_from_slice(name);
        record[264..272].copy_from_slice(&(payload_at as u64).to_le_bytes());
        record[272..276].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        record[276..280].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        let masked = (record.len() as u32) ^ MASK;
        tail.extend_from_slice(&masked.to_le_bytes());
        tail.extend_from_slice(&(masked ^ CHECK).to_le_bytes());
        tail.extend_from_slice(&record);
        let mut header = vec![0u8; 280];
        header[0..4].copy_from_slice(&0xfdfdfeeeu32.to_le_bytes());
        header[4..8].copy_from_slice(&0x0002_0003u32.to_le_bytes());
        header[8..12].copy_from_slice(&((table_at as u32) ^ MASK).to_le_bytes());
        let tag = b"Angelica File Package, Perfect World Co. Ltd.";
        header[20..20 + tag.len()].copy_from_slice(tag);
        header[272..276].copy_from_slice(&0xf00dbeefu32.to_le_bytes());
        tail.extend_from_slice(&header);
        tail.extend_from_slice(&1u32.to_le_bytes());
        tail.extend_from_slice(&0x0002_0003u32.to_le_bytes());
        std::fs::write(&pkx1, &tail).unwrap();

        // The package is over 4 GiB, so the declared size word is the total
        // truncated -- exactly the 1.8.4 situation.
        // The `.pck` is one split, the wide `.pkx` two, and `.pkx1` the tail.
        let total = 3 * split as u64 + tail.len() as u64;
        assert!(total > u32::MAX as u64);
        let mut file = std::fs::OpenOptions::new().write(true).open(&pck).unwrap();
        file.seek(SeekFrom::Start(4)).unwrap();
        file.write_all(&(total as u32).to_le_bytes()).unwrap();
        drop(file);

        let mut package = Package::open(&pck).unwrap();
        assert_eq!(package.continuation_count(), 2);
        assert_eq!(package.len(), total);
        let entries = package.directory().unwrap();
        assert_eq!(entries.len(), 1);
        let data = package.read(&entries[0]).unwrap();
        assert_eq!(
            data, payload,
            "the entry was not read from the file that holds it"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A package cut at `MAX_FILE_PACKAGE` is read as the one package the client
    /// sees: the directory comes out of the `.pkx` half and an entry that spans
    /// the split point is reassembled from both files.
    ///
    /// The 1.5.2 client ships `litmodels.pck` this way (a 2,147,483,392-byte
    /// `.pck` for a 2,859,188,412-byte package). Reading one half alone reported
    /// the archive as unreadable and left all 22,981 scenery models unavailable.
    #[test]
    fn a_split_package_is_read_through_its_pkx_continuation() {
        let straddler = b"ABCDEFGHstraddling-payload-tail";
        let (dir, pck) = split_package("read", straddler);
        let mut package = Package::open(&pck).unwrap();
        assert!(package.is_split(), "a .pck at MAX_FILE_PACKAGE is a split");
        let entries = package.directory().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "litmodels/world/a.bmd");
        let data = package.read(&entries[0]).unwrap();
        assert_eq!(
            data, straddler,
            "the payload was not reassembled across the split"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 1.5.2's `litmodels` shape: the package is longer than a 32-bit offset
    /// can address from its own start, so the trailer's table word has to be
    /// read from the start of the piece that holds it.
    ///
    /// The `.pck` holds the first `MAX_FILE_PACKAGE` bytes and has no trailer;
    /// everything after the split is in the `.pkx`, including the table and the
    /// footer. A package-absolute reading of the word would name a position
    /// past four gigabytes, so only the piece-local reading can work -- and
    /// reading the word as absolute is exactly what rejected the retail archive
    /// with "entry table overlaps footer".
    ///
    /// The `.pkx` is made sparse: `set_len` reserves the length without writing
    /// 711 MB, so this costs nothing on disk and still exercises the real
    /// offsets.
    #[test]
    fn a_table_word_past_four_gigabytes_reads_from_its_own_piece() {
        use std::io::{Seek, SeekFrom};
        let dir = std::env::temp_dir().join(format!(
            "pw-pck-split-retail-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pck = dir.join("litmodels.pck");
        let pkx = dir.join("litmodels.pkx");
        let split = MAX_FILE_PACKAGE;
        let tail_len: u64 = 64 * 1024 * 1024;
        // A package offset at or past the split addresses the `.pkx`: package
        // position `split` is that file's first byte.
        let payload_at = split;
        let payload = b"retail scenery model bytes";

        let mut first = std::fs::File::create(&pck).unwrap();
        first.write_all(&0x4dca23efu32.to_le_bytes()).unwrap();
        let declared = split + tail_len;
        first.write_all(&(declared as u32).to_le_bytes()).unwrap();
        first.write_all(&0x56a089b7u32.to_le_bytes()).unwrap();
        first.set_len(split).unwrap();
        drop(first);

        // Everything after the split, sparse except where it matters.
        let mut tail = std::fs::File::create(&pkx).unwrap();
        let table_at = tail_len - 8 - 288 - 280 - 8;
        let mut record = vec![0u8; 288];
        let name = b"litmodels/world/44/litmodel_1125.bmd";
        record[..name.len()].copy_from_slice(name);
        record[264..272].copy_from_slice(&payload_at.to_le_bytes());
        record[272..276].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        record[276..280].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        let masked = (record.len() as u32) ^ MASK;
        let mut at_table = Vec::new();
        at_table.extend_from_slice(&masked.to_le_bytes());
        at_table.extend_from_slice(&(masked ^ CHECK).to_le_bytes());
        at_table.extend_from_slice(&record);
        // The word is the table's position in *this piece*, not in the package.
        let word = (table_at as u32) ^ MASK;
        let mut header = vec![0u8; 280];
        header[0..4].copy_from_slice(&0xfdfdfeeeu32.to_le_bytes());
        header[4..8].copy_from_slice(&0x0002_0003u32.to_le_bytes());
        header[8..12].copy_from_slice(&word.to_le_bytes());
        let tag = b"Angelica File Package, Perfect World Co. Ltd.";
        header[20..20 + tag.len()].copy_from_slice(tag);
        header[272..276].copy_from_slice(&0xf00dbeefu32.to_le_bytes());
        // The payload lives in this piece, addressed by a package offset that
        // begins where the split does.
        tail.write_all(payload).unwrap();
        tail.seek(SeekFrom::Start(table_at)).unwrap();
        tail.write_all(&at_table).unwrap();
        tail.write_all(&header).unwrap();
        tail.write_all(&1u32.to_le_bytes()).unwrap();
        tail.write_all(&0x0002_0003u32.to_le_bytes()).unwrap();
        tail.set_len(tail_len).unwrap();
        drop(tail);

        let mut package = Package::open(&pck).unwrap();
        assert!(package.is_split());
        assert_eq!(package.len(), declared, "the two pieces add up");
        let entries = package.directory().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "litmodels/world/44/litmodel_1125.bmd");
        assert_eq!(package.read(&entries[0]).unwrap(), payload);
        std::fs::remove_dir_all(dir).unwrap();
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

    /// A file of exactly `MAX_FILE_PACKAGE` with no `.pkx` beside it is the
    /// first half of a package whose second half is missing. Calling that a
    /// corrupt size word would send a reader after a damaged archive that is
    /// merely incomplete.
    #[test]
    fn a_pck_at_max_file_package_without_its_pkx_names_the_missing_half() {
        let whole = package(0x0002_0003, &[("a.txt", b"x")], false);
        let mut bytes = vec![0u8; MAX_FILE_PACKAGE as usize];
        bytes[..whole.len()].copy_from_slice(&whole);
        // The header declares a package larger than the one file on disk, which
        // is exactly what a split looks like from this side.
        let declared = (MAX_FILE_PACKAGE + 4096) as u32;
        bytes[4..8].copy_from_slice(&declared.to_le_bytes());
        let path = write_temp(&bytes, "truncated-split");
        let mut file = std::fs::File::open(&path).unwrap();
        let error = directory(&mut file).unwrap_err();
        drop(file);
        std::fs::remove_file(path).unwrap();
        assert!(error.contains("split package"), "unexpected error: {error}");
        assert!(error.contains("missing"), "unexpected error: {error}");
    }
}
