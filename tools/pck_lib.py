"""Python port of src/pck.rs (retail v2 PCK reader), for archive surveys."""
import struct, zlib

class EncryptedPackage(Exception):
    """A well-formed package this reader cannot read, as opposed to a broken one.

    Surveys catch this and count it as *skipped*, so "unsupported" in a census
    means "this build lacks the feature" and never "the archive is damaged".
    """


MASK = 0xa8937462
CHECK = 0x59374231

def directory(path):
    with open(path, 'rb') as handle:
        version, entries = _directory(handle)
    return version, entries


def _directory(f):
    size = len(f.read())
    def rd(pos, n):
        f.seek(pos); b = f.read(n)
        assert len(b) == n, 'truncated'
        return b
    U = lambda b, p: struct.unpack_from('<I', b, p)[0]
    safe = rd(0, 12)
    assert U(safe, 0) == 0x4dca23ef and U(safe, 8) == 0x56a089b7, 'bad safe header'
    end = U(safe, 4)
    assert 292 <= end <= size, 'bad end %d' % end
    header = rd(end - 280, 280)
    assert U(header, 0) == 0xfdfdfeee and U(header, 268) == 0xf00dbeef, 'bad guard'
    version = U(header, 276)
    assert version in (0x20002, 0x20001), 'version %08x' % version
    # A package with this bit set is *not* corrupt: it is a well-formed archive
    # whose directory is enciphered, and this reader has no deciphering step.
    # Raising a distinct, named error keeps a survey from counting a shipped
    # archive as broken -- the distinction `src/pck.rs` draws with
    # `ENCRYPTED_REFUSAL`.
    if U(header, 12) & 0x80000000:
        raise EncryptedPackage(
            'encrypted package unsupported: revision %08x sets dwFlags bit 31'
            % version)
    count = U(header, 272)
    pos = U(header, 8) ^ MASK
    out = []
    for _ in range(count):
        h = rd(pos, 8)
        ln = U(h, 0) ^ MASK
        assert (U(h, 4) ^ MASK ^ CHECK) == ln, 'record len check'
        raw = rd(pos + 8, ln)
        data = raw if ln == 276 else zlib.decompress(raw)
        assert len(data) == 276, 'decompressed %d' % len(data)
        zero = data[:260].find(b'\0')
        if zero < 0: zero = 260
        name = data[:zero].decode('gbk', 'replace').replace('\\', '/')
        out.append((name, U(data, 260), U(data, 264), U(data, 268)))
        pos += 8 + ln
    return version, out

def read_entry(path, entry):
    name, off, usize, clen = entry
    with open(path, 'rb') as f:
        f.seek(off); b = f.read(clen)
    return zlib.decompress(b) if clen < usize else b
