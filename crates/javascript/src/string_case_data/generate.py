"""Generate the Unicode 17.0.0 Final_Sigma range boundaries.

Usage: python generate.py PATH_TO_DERIVED_CORE_PROPERTIES
Source: https://www.unicode.org/Public/17.0.0/ucd/DerivedCoreProperties.txt
License: ../regexp/data/LICENSE.txt (Unicode License V3)
"""

from hashlib import sha256
from pathlib import Path
import struct
import sys

SOURCE_HASH = "24c7fed1195c482faaefd5c1e7eb821c5ee1fb6de07ecdbaa64b56a99da22c08"


def main(source_path):
    source = source_path.read_bytes()
    if sha256(source).hexdigest() != SOURCE_HASH:
        raise ValueError("Expected the pinned Unicode 17.0.0 DerivedCoreProperties.txt")
    states = bytearray(0x110000)
    properties = {"Cased": 1, "Case_Ignorable": 2}
    for line in source.decode("utf-8").splitlines():
        body = line.partition("#")[0].strip()
        if not body:
            continue
        extent, name, *_ = (part.strip() for part in body.split(";"))
        state = properties.get(name)
        if state is None:
            continue
        lower, sep, upper = extent.partition("..")
        first, last = int(lower, 16), int(upper if sep else lower, 16)
        for point in range(first, last + 1):
            # Case_Ignorable is skipped even when a point is also Cased.
            states[point] = max(states[point], state)

    data = bytearray()
    previous = None
    for point, state in enumerate(states):
        if state != previous:
            data.extend(struct.pack("<I", (point << 2) | state))
            previous = state
    Path(__file__).with_name("context-17.0.0.bin").write_bytes(data)


if __name__ == "__main__":
    main(Path(sys.argv[1]))
