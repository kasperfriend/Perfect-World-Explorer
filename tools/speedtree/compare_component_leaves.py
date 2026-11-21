#!/usr/bin/env python3
"""Compare PWCOMP02 leaf records to lod-explicit-probe automatic hashes.

FNV hashes cover the leaf count, centers, normals, cluster IDs, UVs and
fixed-camera map coordinates, not material state or animated geometry.
"""
import argparse
import csv
import struct
from pathlib import Path
from compare_lod_counts import read_counts
from survey_explicit_lods import check_rows


def leaf_hashes(data):
    offset = 0
    def take(n):
        nonlocal offset
        if n < 0 or offset + n > len(data):
            raise ValueError('truncated PWCOMP02')
        out = data[offset:offset + n]
        offset += n
        return out
    def u32():
        return struct.unpack('<I', take(4))[0]
    if take(8) != b'PWCOMP02':
        raise ValueError('unexpected cache version')
    take(12)  # DLL hash, source hash and type ID (checked by Rust reader).
    for _ in range(2):
        n = u32()
        if n > 4096: raise ValueError('texture name too long')
        take(n)
    for _ in range(2):
        n = u32()
        if n > 64: raise ValueError('LOD limit')
        for _ in range(n):
            vertices = u32()
            if vertices > 65535: raise ValueError('vertex limit')
            take(vertices * 32)
            indices = u32()
            if indices > 3_000_000 or indices % 3: raise ValueError('index limit')
            take(indices * 4)
    result = []
    n = u32()
    if n > 64: raise ValueError('LOD limit')
    for _ in range(n):
        count = u32()
        if count > 65535: raise ValueError('leaf limit')
        centers, normals, clusters, cards = [], [], [], []
        for _ in range(count):
            centers.append(take(12)); normals.append(take(12))
            cluster = u32()
            if cluster > 255: raise ValueError('cluster limit')
            clusters.append(bytes([cluster])); cards.append(take(96))
        h = 14695981039346656037
        for chunk in (struct.pack('<H', count), *centers, *normals, *clusters, *cards):
            for byte in chunk:
                h = ((h ^ byte) * 1099511628211) & ((1 << 64) - 1)
        result.append(f'{h:016x}')
    if offset != len(data): raise ValueError('trailing PWCOMP02 bytes')
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('archives', type=Path)
    p.add_argument('traces', type=Path)
    p.add_argument('counts', type=Path)
    p.add_argument('--missing', type=int, nargs='*', default=[],
                   help='IDs expected to have failed; missing others are errors')
    args = p.parse_args()
    counts = read_counts(args.counts, 'type', ('branch', 'frond', 'leaf'))
    missing = set(args.missing)
    if len(missing) != len(args.missing) or not missing <= set(range(59)):
        raise ValueError('invalid missing type IDs')
    levels = 0
    for i in range(59):
        path = args.archives / f'{i:03}.pwc'
        if i in missing:
            if path.exists(): raise ValueError(f'type {i}: unexpectedly present')
            continue
        text = (args.traces / f'{i:03}.tsv').read_text()
        check_rows(text, counts[i])
        rows = csv.DictReader(text.splitlines(), delimiter='\t')
        expected = [r['auto_hash'] for r in rows if r['component'] == '2']
        actual = leaf_hashes(path.read_bytes())
        if actual != expected: raise ValueError(f'type {i}: leaf geometry mismatch')
        levels += len(actual)
    print(f'{59-len(missing)} types, {levels} leaf levels matched')


if __name__ == '__main__':
    main()
