"""Generate the CLDR fields absent from ICU4X DisplayNames.

Pass the directory containing the official cldr-dates-full, cldr-localenames-full,
and cldr-numbers-full npm tarballs for version 49.0.0. Run from any directory.
"""

import argparse
import base64
import zlib
import collections
import json
from pathlib import Path
import tarfile

VERSION = "49.0.0"
TARGET = Path(__file__).resolve().parent
FIELDS = {
    "era": "era", "year": "year", "quarter": "quarter", "month": "month",
    "weekOfYear": "week", "weekday": "weekday", "day": "day", "dayPeriod": "dayperiod",
    "hour": "hour", "minute": "minute", "second": "second", "timeZoneName": "zone",
}
CALENDARS = {"gregorian": "gregory", "ethiopic-amete-alem": "ethioaa"}


def records(group, source):
    if group == "dates":
        fields = source["dates"]["fields"]
        for width in ["long", "short", "narrow"]:
            for field, key in FIELDS.items():
                item = fields.get(key if width == "long" else f"{key}-{width}", fields.get(key, {}))
                if "displayName" in item:
                    yield f"{width}:{field}", item["displayName"]
    elif group == "numbers":
        for key, row in source["numbers"]["currencies"].items():
            if "displayName" in row:
                yield key, row["displayName"]
    else:
        for key, name in source["localeDisplayNames"].get("types", {}).get("calendar", {}).items():
            if isinstance(name, str) and "-alt-" not in key:
                yield CALENDARS.get(key, key), name


def generate(path):
    data = collections.defaultdict(dict)
    for group, filename in [("dates", "dateFields"), ("numbers", "currencies"), ("localenames", "localeDisplayNames")]:
        pkg = f"cldr-{group}-full"
        archive = path / f"{pkg}-{VERSION}.tgz"
        with tarfile.open(archive) as stream:
            info = json.load(stream.extractfile("package/package.json"))
            if (info["name"], info["version"]) != (pkg, VERSION):
                raise ValueError(f"Unexpected data version in {archive}")
            for row in stream:
                if row.name.startswith("package/main/") and row.name.endswith(f"/{filename}.json"):
                    loc = row.name.split("/")[2]
                    source = json.load(stream.extractfile(row))["main"][loc]
                    data[loc].update(records(group, source))
    text = bytearray()
    index = []
    for locale, rows in sorted(data.items()):
        parent = locale.rpartition("-")[0]
        while parent and parent not in data:
            parent = parent.rpartition("-")[0]
        # Country-specific files share most of their CLDR language data.
        inherited = data.get(parent, {})
        records_utf8 = bytearray()
        offset = len(text)
        for key in sorted(set(rows) | set(inherited)):
            value = rows.get(key, "")
            if value != inherited.get(key, ""):
                if any(c in value for c in "\n\r\t"):
                    raise ValueError(f"Invalid record {locale} {key}")
                records_utf8.extend(f"{key}\t{value}\n".encode())
        if records_utf8:
            text.extend(base64.b64encode(zlib.compress(records_utf8, level=9)))
            index.append(f'    ("{locale}", {offset}, {len(text)}),\n')
            text.append(10)
    # Fixed-size chunks keep the generated assets small enough for GitHub editors.
    chunks = []
    for start in range(0, len(text), 160_000):
        name = f"display_names_{len(chunks)}.b64"
        (TARGET / name).write_bytes(text[start:start + 160_000])
        chunks.append(f'include_str!("{name}")')
    (TARGET / "display_index.rs").write_text(
        "// Generated from CLDR 49 by generate_display_names.py.\n"
        "pub(super) const NAMES: &str = concat!(" + ", ".join(chunks) + ");\n"
        "pub(super) const LOCALES: &[(&str, usize, usize)] = &[\n" + "".join(index) + "];\n"
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    generate(parser.parse_args().directory)
