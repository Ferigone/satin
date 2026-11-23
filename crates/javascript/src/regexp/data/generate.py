"""Reproduce the RegExp tables from Unicode 17.0.0's UCD text files."""
from collections import defaultdict
from hashlib import sha256
from pathlib import Path
import struct
import sys

SOURCES = {
    "CaseFolding.txt": "ff8d8fefbf123574205085d6714c36149eb946d717a0c585c27f0f4ef58c4183",
    "UnicodeData.txt": "2e1efc1dcb59c575eedf5ccae60f95229f706ee6d031835247d843c11d96470c",
    "SpecialCasing.txt": "efc25faf19de21b92c1194c111c932e03d2a5eaf18194e33f1156e96de4c9588",
    "DerivedCoreProperties.txt": "24c7fed1195c482faaefd5c1e7eb821c5ee1fb6de07ecdbaa64b56a99da22c08",
}

def fields(source, name):
    data = (source / name).read_bytes()
    if sha256(data).hexdigest() != SOURCES[name]:
        raise ValueError(f"{name} does not match the pinned Unicode 17.0.0 data")
    for line in data.decode("utf-8").splitlines():
        body = line.partition("#")[0].strip()
        if body:
            yield tuple(part.strip() for part in body.split(";"))

def write_case(path, mappings):
    """Map each point to its contiguous equivalence group in a point array."""
    groups = defaultdict(set)
    for point, canonical in mappings.items():
        groups[canonical].add(point)
        groups[canonical].add(canonical)
    index = []
    points = []
    for _, group in sorted(groups.items()):
        index.extend((point, len(points), len(group)) for point in group)
        points.extend(sorted(group))
    index.sort()
    assert len({row[0] for row in index}) == len(index)
    data = struct.pack("<I", len(index))
    for point, offset, count in index:
        data += struct.pack("<IHH", point, offset, count)
    data += struct.pack(f"<{len(points)}I", *points)
    path.write_bytes(data)

def main(source, out):
    folding = {}
    for point, kind, replacement, *_ in fields(source, "CaseFolding.txt"):
        if kind in ("C", "S"):
            folding[int(point, 16)] = int(replacement, 16)
    write_case(out / "case-fold.bin", folding)
    uppercase = {
        int(row[0], 16): int(row[12], 16) for row in fields(source, "UnicodeData.txt") if row[12]
    }
    for point, _, _, upper, condition, *_ in fields(source, "SpecialCasing.txt"):
        if not condition:
            uppercase[int(point, 16)] = int(upper, 16) if " " not in upper else int(point, 16)
    legacy = {
        point: upper for point, upper in uppercase.items()
        if point <= 0xFFFF and upper <= 0xFFFF and point != upper
        and not (point >= 128 and upper < 128)
    }
    write_case(out / "legacy-case.bin", legacy)
    for prop, filename in (("ID_Start", "id-start.bin"), ("ID_Continue", "id-continue.bin")):
        data = bytearray()
        for extent, name, *_ in fields(source, "DerivedCoreProperties.txt"):
            if name != prop:
                continue
            first, sep, last = extent.partition("..")
            data.extend(struct.pack("<II", int(first, 16), int(last if sep else first, 16)))
        (out / filename).write_bytes(data)

if __name__ == "__main__":
    main(Path(sys.argv[1]), Path(__file__).parent)
