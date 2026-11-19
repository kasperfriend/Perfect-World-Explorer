"""Python port of `src/pck.rs` and `src/angelica_pck.rs`, for archive surveys.

Two containers ship under the `.pck` name and both are handled here, because a
survey that reads only one reports the other as damaged:

* **retail** — a 12-byte `4dca23ef <size> <tag>` prefix, a forward directory,
  and a trailing `FILEHEADER` whose revision word says how the records are laid
  out. Revisions `0x20001`, `0x20002` (272-byte trailer, 276-byte records) and
  `0x20003` (280-byte trailer, 288-byte records, 64-bit offsets) all ship.
* **Angelica** — the same prefix and the same trailer, but the directory is
  **zlib-compressed and walked backwards** from the trailer.

Which one a file is cannot be guessed from its name: `configs.pck` is retail in
1.8.4 and Angelica in 1.5.2. `directory()` tries the container the trailer
revision implies and falls back to the other.

Measured on 18 November, all three archives now read:

| client | pack | container | revision | entries |
| --- | --- | --- | --- | ---: |
| 1.5.2 | `configs.pck` | Angelica | `0x00020002` | 67 |
| 1.5.2 | `grasses.pck` | Angelica | `0x00020002` | 103 |
| 1.8.4 | `configs.pck` | retail | `0x00020003` | 1,057 |

The 1.5.2 archives carry the tag word `0x56a089b7` and the guard pair
`0xa508bdc4`/`0x853567c8`; 1.8.4 carries tag **0** and `0xfdfdfeee`/`0xf00dbeef`.
Both pairs and both tags are accepted, because both are shipped and measured.
"""

import struct
import zlib


class EncryptedPackage(Exception):
    """A well-formed package this reader cannot read, as opposed to a broken one.

    Surveys catch this and count it as *skipped*, so "unsupported" in a census
    means "this build lacks the feature" and never "the archive is damaged".
    """


MASK = 0xa8937462
CHECK = 0x59374231

PCK_MAGIC = 0x4DCA23EF
TAG_152 = 0x56A089B7
TAGS = (TAG_152, 0)

GUARD_0, GUARD_1 = 0xFDFDFEEE, 0xF00DBEEF
ANGELICA_GUARD_0, ANGELICA_GUARD_1 = 0xA508BDC4, 0x853567C8
GUARDS = ((ANGELICA_GUARD_0, ANGELICA_GUARD_1), (GUARD_0, GUARD_1))

# revision -> (header size, entry size, offset_at, offset_width, size_at,
#              compressed_at, guard_byte1, flags_at)
#
# `flags_at` moves with the revision, and reading it at a fixed 12 reports a
# 1.8.4 package as enciphered because that word is the description's first
# bytes (0xffffffff) rather than `dwFlags`.
REVISIONS = {
    0x20003: (280, 288, 264, 8, 272, 276, 272, 16),
    0x20002: (272, 276, 260, 4, 264, 268, 268, 12),
    0x20001: (272, 276, 260, 4, 264, 268, 268, 12),
}


def directory(path):
    """Return `(revision, entries)` for either container.

    Each entry is `(name, offset, size, compressed_size)`.
    """
    with open(path, "rb") as handle:
        data = handle.read()
    size = len(data)

    def U(pos):
        return struct.unpack_from("<I", data, pos)[0]

    if size < 16:
        raise ValueError("truncated package")
    if U(0) != PCK_MAGIC:
        raise ValueError("bad package magic %08x" % U(0))
    if U(8) not in TAGS:
        raise ValueError(
            "unsupported package tag %08x; expected 56a089b7 (1.5.2) or 00000000 (1.8.4)"
            % U(8)
        )
    version = U(size - 4)
    if version not in REVISIONS:
        raise ValueError("unsupported package revision %08x" % version)
    (
        header,
        entry,
        offset_at,
        offset_width,
        size_at,
        compressed_at,
        guard_byte1,
        flags_at,
    ) = REVISIONS[version]
    trailer = size - (header + 8)
    if trailer < 12:
        raise ValueError("truncated trailer")
    guards = (U(trailer), U(trailer + guard_byte1))
    if guards not in GUARDS:
        raise ValueError(
            "unsupported package guard %08x/%08x" % guards
        )

    # `dwFlags` bit 31 is a property of the *package*, so it is decided from the
    # trailer before either container is attempted. Checking it inside the
    # retail reader let an enciphered package be reported as a malformed
    # Angelica one instead of as a feature this build lacks -- which is the
    # distinction `EncryptedPackage` exists to preserve.
    if U(trailer + flags_at) & 0x80000000:
        raise EncryptedPackage(
            "encrypted package unsupported: revision %08x sets dwFlags bit 31"
            % version
        )

    # Which container it is follows from the trailer revision, but the two
    # disagree often enough (`configs.pck` is retail in 1.8.4 and Angelica in
    # 1.5.2) that a failed parse is retried as the other rather than reported.
    errors = []
    for reader in (_angelica_records, _retail_records):
        try:
            return version, reader(data, size, header, entry, offset_at,
                                   offset_width, size_at, compressed_at, version)
        except EncryptedPackage:
            raise
        except Exception as error:  # noqa: BLE001 - the other container is tried
            errors.append(f"{reader.__name__}: {error}")
    raise ValueError("; ".join(errors))


def _retail_records(data, size, header, entry, offset_at, offset_width,
                    size_at, compressed_at, version):
    count = struct.unpack_from("<I", data, size - (header + 8) + header)[0]
    if count > 1_000_000:
        raise ValueError("entry count %d exceeds limit" % count)
    out = []
    for i in range(count):
        pos = 12 + i * entry
        record = data[pos:pos + entry]
        if len(record) != entry:
            raise ValueError("truncated record %d" % i)
        zero = record[:260].find(b"\0")
        if zero < 0:
            zero = 260
        name = record[:zero].decode("gbk", "replace").replace("\\", "/")
        if offset_width == 8:
            offset = struct.unpack_from("<Q", record, offset_at)[0]
        else:
            offset = struct.unpack_from("<I", record, offset_at)[0]
        out.append((
            name,
            offset,
            struct.unpack_from("<I", record, size_at)[0],
            struct.unpack_from("<I", record, compressed_at)[0],
        ))
    return out


def _angelica_records(data, size, header, entry, offset_at, offset_width,
                      size_at, compressed_at, version):
    trailer = size - (header + 8)
    count = struct.unpack_from("<I", data, trailer + header)[0]
    if count > 1_000_000:
        raise ValueError("Angelica entry count %d exceeds limit" % count)
    end = trailer
    reversed_records = []
    for index in range(count):
        record, start = _record_ending_at(data, end, entry)
        if record is None:
            raise ValueError(
                "no Angelica record ends at the expected offset (record %d)" % index
            )
        zero = record[:260].find(b"\0")
        if zero < 0:
            zero = 260
        name = record[:zero].decode("gbk", "replace").replace("\\", "/")
        if offset_width == 8:
            offset = struct.unpack_from("<Q", record, offset_at)[0]
        else:
            offset = struct.unpack_from("<I", record, offset_at)[0]
        usize = struct.unpack_from("<I", record, size_at)[0]
        compressed = struct.unpack_from("<I", record, compressed_at)[0]
        if offset + compressed > size or compressed > usize:
            raise ValueError("invalid Angelica entry bounds: %s" % name)
        reversed_records.append((name, offset, usize, compressed))
        if start < 8:
            raise ValueError("Angelica record prefix out of bounds")
        end = start - 8
    reversed_records.reverse()
    return reversed_records


def _record_ending_at(data, end, record_len):
    """Mirror of `angelica_pck::record_ending_at`."""
    for span in (1 << 16, 1 << 19, 1 << 22):
        low = max(0, end - span)
        for start in range(end - 1, low - 1, -1):
            raw = data[start:end]
            if len(raw) < 2:
                continue
            if (raw[0] & 0x0F) != 8 or (raw[0] >> 4) > 7 or (raw[1] & 0x20) != 0:
                continue
            if ((raw[0] << 8) | raw[1]) % 31 != 0:
                continue
            try:
                out = zlib.decompressobj().decompress(raw)
            except zlib.error:
                continue
            if len(out) == record_len:
                return out, start
    return None, end


def read_entry(path, entry):
    name, off, usize, clen = entry
    del name
    with open(path, "rb") as handle:
        handle.seek(off)
        blob = handle.read(clen)
    return zlib.decompress(blob) if clen < usize else blob
