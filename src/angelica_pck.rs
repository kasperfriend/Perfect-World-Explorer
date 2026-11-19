//! Reader for the Angelica File Package container used by `configs.pck` and
//! `shaders.pck`.
//!
//! The retail `textures.pck` is a v2 PCK and is read by [`crate::pck`]. The
//! configuration and shader packages shipped beside it use a different trailer
//! and a different directory layout. The viewer used to open `configs.pck`
//! with the retail reader, which rejected the guard and silently fell back to
//! defaults; that made `element_client.cfg`, `autoscene.cfg` and `sky.lst`
//! unreadable. This module reads them.
//!
//! Layout, from `tools/extract-angelic-pck.py` and the client's own loader:
//!
//! | offset | content |
//! | --- | --- |
//! | `0x000` | `u32 0x4dca23ef`, file size, `u32 0x56a089b7` |
//! | `0x00c` | file payloads; stored raw or bare zlib |
//! | directory | `count` records, each an 8-byte prefix then a zlib stream of one 276-byte record, stored backwards |
//! | `-280` | 280-byte trailer: guard `0xfdfdfeee`, version, description, guard `0xf00dbeef` at `+268`, count at `+272`, version at `+276` |
//!
//! The directory is not a forward table: starting at the trailer, each record
//! is the zlib stream that ends exactly at the current position. The 8-byte
//! prefix before the stream is skipped, and the records are collected in
//! reverse and then reversed into stored order. A stored payload is raw at
//! `offset`; a compressed one is a bare zlib stream (`compressed < size`).

use crate::pck::Entry;
use flate2::{Decompress, FlushDecompress};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

const WINDOWS: [usize; 3] = [1 << 16, 1 << 19, 1 << 22];
/// `u32 0x4dca23ef`, file size, `u32 0x56a089b7`; the first payload follows it.
const ANGELICA_PREFIX_LEN: usize = 12;
const MIN_COMPRESSED_RECORD: usize = 8;

fn u32le(b: &[u8], p: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4)
            .ok_or("truncated Angelica header")?
            .try_into()
            .unwrap(),
    ))
}

/// Find the zlib stream that ends exactly at `end` and inflates to 276 bytes.
/// Mirrors `_stream_ending_at`: scan backwards for a zlib header (CM=8,
/// window <= 7) and accept the first stream that consumes exactly the bytes up
/// to `end` and produces one record.
fn record_ending_at(
    blob: &[u8],
    end: usize,
    decomp: &mut Decompress,
) -> Result<([u8; 276], usize), String> {
    let mut out = [0u8; 276];
    let mut searched_low = end.saturating_sub(MIN_COMPRESSED_RECORD).min(blob.len());
    for span in WINDOWS {
        let low = end.saturating_sub(span);
        let mut start = searched_low;
        while start > low {
            start -= 1;
            let raw = &blob[start..end];
            if !(raw.len() >= 2
                && (raw[0] & 0x0f) == 8
                && (raw[0] >> 4) <= 7
                && (raw[1] & 0x20) == 0
                && ((u16::from(raw[0]) << 8) | u16::from(raw[1])) % 31 == 0)
            {
                continue;
            }
            decomp.reset(true);
            let Ok(status) = decomp.decompress(raw, &mut out, FlushDecompress::Finish) else {
                continue;
            };
            if status == flate2::Status::StreamEnd
                && decomp.total_out() == 276
                && decomp.total_in() == (end - start) as u64
            {
                return Ok((out, start));
            }
        }
        searched_low = low;
    }
    Err("no Angelica record ends at the expected offset".into())
}

/// Read the Angelica package directory. The returned entries are the same
/// [`crate::pck::Entry`] values the retail reader produces, so
/// [`crate::pck::read_entry`] can read their payloads unchanged.
pub fn directory(f: &mut File) -> Result<Vec<Entry>, String> {
    let file_len = f.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    if file_len < (ANGELICA_PREFIX_LEN + 280) as u64 {
        return Err("Angelica package too small".into());
    }
    f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut prefix = [0u8; 12];
    f.read_exact(&mut prefix).map_err(|e| e.to_string())?;
    let magic = u32le(&prefix, 0)?;
    let size = u32le(&prefix, 4)?;
    let tag = u32le(&prefix, 8)?;
    if magic != 0x4dca23ef || tag != 0x56a089b7 || size as u64 != file_len {
        return Err("bad Angelica prefix".into());
    }
    let tail_start = file_len.saturating_sub(64 * 1024 * 1024);
    f.seek(SeekFrom::Start(tail_start))
        .map_err(|e| e.to_string())?;
    let mut blob = Vec::with_capacity((file_len - tail_start) as usize);
    f.read_to_end(&mut blob).map_err(|e| e.to_string())?;
    // The trailer is the retail package's oldest revision, byte for byte: a
    // 272-byte `FILEHEADER`, the count, then the version word. Both shipped
    // archives state `0x00020002` (1.5.2's `configs.pck`) or `0x00020001`
    // (`shaders.pck`), which is why the layout, the field offsets and the guard
    // pair all come from the one table in `pck` instead of a second copy here.
    //
    // The `0xa508bdc4` / `0x853567c8` pair that an earlier version also accepted
    // is gone: it appears in no staged archive, `pck` never accepts it, and two
    // readers disagreeing about which guards are valid is the thing this
    // consolidation removes. An archive carrying it is refused by name below.
    //
    // The version word sits in the last four bytes, exactly as in a retail
    // package, and it is what says how long the trailer is -- so it is read
    // first, the same order `pck::read_trailer` uses.
    let version = u32le(&blob, blob.len() - 4)?;
    let Some(rev) = crate::pck::angelica_revision(version) else {
        return Err(format!(
            "unsupported Angelica trailer: version {version:08x} is not a package \
             revision this build reads"
        ));
    };
    // `header` is `sizeof(FILEHEADER*)`; the count follows it and the version
    // word follows the count.
    let trailer = blob.len() - (rev.header + 8);
    let header = &blob[trailer..trailer + rev.header];
    let guard = u32le(header, 0)?;
    let guard2 = u32le(header, rev.guard_byte1)?;
    if guard != crate::pck::GUARD_0 || guard2 != crate::pck::GUARD_1 {
        return Err(format!(
            "unsupported Angelica trailer: guard {guard:08x}/{guard2:08x} is not the \
             package guard pair {:08x}/{:08x}",
            crate::pck::GUARD_0,
            crate::pck::GUARD_1
        ));
    }
    let count = u32le(&blob, trailer + rev.header)? as usize;
    // The same bound as the retail reader, and for the same reason: the largest
    // staged Angelica package is 1.5.2's `configs.pck` at 58 entries, so this
    // only ever fires on a damaged count.
    if count > crate::pck::ENTRY_LIMIT {
        return Err(format!(
            "Angelica entry count {count} exceeds limit {}",
            crate::pck::ENTRY_LIMIT
        ));
    }
    let mut pos = trailer;
    let mut reversed = Vec::with_capacity(count);
    let mut decomp = Decompress::new(true);
    for _ in 0..count {
        let (record, start) = record_ending_at(&blob, pos, &mut decomp)?;
        let zero = record[..260].iter().position(|&x| x == 0).unwrap_or(260);
        let (decoded, _, _) = encoding_rs::GBK.decode(&record[..zero]);
        let name = decoded.replace('\\', "/");
        let offset = u32le(&record, 260)?;
        let usize_ = u32le(&record, 264)?;
        let compressed = u32le(&record, 268)?;
        if offset as u64 + compressed as u64 > file_len
            || usize_ > 512 * 1024 * 1024
            || compressed > usize_
        {
            return Err(format!("invalid Angelica entry bounds: {name}"));
        }
        reversed.push(Entry {
            name,
            offset: offset as u64,
            size: usize_,
            compressed,
        });
        // The 8-byte prefix before the zlib stream is skipped.
        if start < 8 {
            return Err("Angelica record prefix out of bounds".into());
        }
        pos = start - 8;
    }
    // The records were walked backwards from the trailer, so after `count` of
    // them the cursor should sit exactly on the end of the last payload. A gap
    // means the framing was misread -- a wrong count, a record stream that is
    // not where it was expected -- and the entries that came out would not be
    // the archive's. The shipped packs land exactly, so this costs no coverage.
    // Payloads start right after the 12-byte prefix, so that is where an
    // empty package -- a real one is exactly 12 + 280 bytes -- leaves the run.
    // Checking it unconditionally is what makes a tampered count of 0 a
    // refusal instead of a plausible-looking empty archive.
    let payload_end = reversed
        .iter()
        .map(|e| e.offset as u64 + e.compressed as u64)
        .max()
        .unwrap_or(ANGELICA_PREFIX_LEN as u64);
    if pos as u64 != payload_end {
        return Err(format!(
            "Angelica record run ends at {pos} but the payloads end at {payload_end}"
        ));
    }
    reversed.reverse();
    Ok(reversed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a one-entry Angelica package around a stored 276-byte record.
    fn package(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut record = vec![0u8; 276];
        let name_bytes = name.as_bytes();
        record[..name_bytes.len()].copy_from_slice(name_bytes);
        record[260..264].copy_from_slice(&12u32.to_le_bytes());
        record[264..268].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        record[268..272].copy_from_slice(&(payload.len() as u32).to_le_bytes());

        // A stored zlib stream of the record. `flate2` can produce one with
        // `ZlibEncoder`; the reader only needs a valid stream that ends at the
        // record boundary.
        use flate2::write::ZlibEncoder;
        use std::io::Write;
        let mut encoder = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&record).unwrap();
        let stream = encoder.finish().unwrap();

        let mut out = Vec::new();
        out.extend_from_slice(&0x4dca23efu32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // patched below
        out.extend_from_slice(&0x56a089b7u32.to_le_bytes());
        out.extend_from_slice(payload);
        let dir_at = out.len();
        out.extend_from_slice(&[0u8; 8]); // 8-byte record prefix
        out.extend_from_slice(&stream);
        let mut header = vec![0u8; 280];
        header[0..4].copy_from_slice(&crate::pck::GUARD_0.to_le_bytes());
        header[4..8].copy_from_slice(&0x20002u32.to_le_bytes());
        header[268..272].copy_from_slice(&crate::pck::GUARD_1.to_le_bytes());
        header[272..276].copy_from_slice(&1u32.to_le_bytes());
        header[276..280].copy_from_slice(&0x20002u32.to_le_bytes());
        out.extend_from_slice(&header);
        let total = out.len() as u32;
        out[4..8].copy_from_slice(&total.to_le_bytes());
        let _ = dir_at;
        out
    }

    #[test]
    fn reads_an_angelica_package_and_its_payload() {
        let stored = b"3\r\n0\r\n";
        let pkg = package("configs/autofamilyconfigs/sky.lst", stored);
        let path = std::env::temp_dir().join("pw-angelica-test.pck");
        std::fs::write(&path, &pkg).unwrap();
        let mut file = File::open(&path).unwrap();
        let entries = directory(&mut file).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "configs/autofamilyconfigs/sky.lst");
        assert_eq!(entries[0].size, stored.len() as u32);
        assert_eq!(entries[0].compressed, stored.len() as u32);
        let payload = crate::pck::read_entry(&mut file, &entries[0]).unwrap();
        assert_eq!(payload, stored);
        std::fs::remove_file(&path).ok();
    }

    /// The record run is walked backwards from the trailer, so `count` decides
    /// how far back it goes. A count that is too small leaves the cursor short
    /// of the prefix and a count that is too large runs past it; either way the
    /// records that came out are not the archive's directory. Before the run
    /// was checked against the payloads, a count of 0 was reported as a valid
    /// empty archive.
    #[test]
    fn a_record_run_that_does_not_land_on_the_prefix_is_refused() {
        let pkg = package("configs/sky.lst", b"3\r\n0\r\n");
        let count_at = pkg.len() - 280 + 272;
        let count = u32::from_le_bytes(pkg[count_at..count_at + 4].try_into().unwrap());
        assert_eq!(count, 1, "the fixture has one record");
        for wrong in [0u32, count - 1, count + 1, count + 7] {
            let mut tampered = pkg.clone();
            tampered[count_at..count_at + 4].copy_from_slice(&wrong.to_le_bytes());
            let path = std::env::temp_dir().join(format!("pw-angelica-count-{wrong}.pck"));
            std::fs::write(&path, &tampered).unwrap();
            let mut file = File::open(&path).unwrap();
            let result = directory(&mut file);
            assert!(
                result.is_err(),
                "a count of {wrong} must not yield a directory, got {:?}",
                result.map(|e| e.len())
            );
            std::fs::remove_file(&path).ok();
        }
    }
}
