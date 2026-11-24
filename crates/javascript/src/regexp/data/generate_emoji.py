"""Build the regexp string-property index from Unicode 17.0 emoji data.

Download emoji-sequences.txt and emoji-zwj-sequences.txt from
https://www.unicode.org/Public/17.0.0/emoji/ into the input directory.
"""
from hashlib import sha256
from pathlib import Path
from struct import pack
import sys

SOURCES = {
    "emoji-sequences.txt": "12cc8267dc33cbd11ed32bcf6fc5dc2ad9c7a77bae1bdfba2f41b1b9b3ead8dd",
    "emoji-zwj-sequences.txt": "5b25441daed2322b068c5e70cda522946a4f0274df864445a1965a92e5fc5cad",
}
PROPERTIES = [
    "Basic_Emoji", "Emoji_Keycap_Sequence", "RGI_Emoji_Flag_Sequence",
    "RGI_Emoji_Modifier_Sequence", "RGI_Emoji_Tag_Sequence", "RGI_Emoji_ZWJ_Sequence",
]


def main(source):
    strings = {}
    for name, digest in SOURCES.items():
        data = (source / name).read_bytes()
        if sha256(data).hexdigest() != digest:
            raise ValueError(f"{name} does not match Unicode 17.0")
        for line in data.decode().splitlines():
            text = line.partition("#")[0].strip()
            if not text:
                continue
            points, prop, _ = (part.strip() for part in text.split(";", 2))
            if ".." in points:
                first, last = (int(n, 16) for n in points.split(".."))
                values = ((n,) for n in range(first, last + 1))
            else:
                values = [tuple(int(n, 16) for n in points.split())]
            for value in values:
                strings[value] = strings.get(value, 0) | (1 << PROPERTIES.index(prop))
    table = bytearray()
    offsets = []
    for value, kind in sorted(strings.items()):
        offsets.append(len(table))
        table.append(kind)
        for point in value:
            table.extend(point.to_bytes(3, "big"))
    offsets.append(len(table))
    output = pack("<II", len(strings), max(map(len, strings)))
    output += pack(f"<{len(offsets)}I", *offsets) + table
    Path(__file__).with_name("emoji-sequences.bin").write_bytes(output)


if __name__ == "__main__":
    main(Path(sys.argv[1]))
