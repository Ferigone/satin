"""Rebuild the offline property inventory from pinned Webref data and a reference browser."""
import argparse
import hashlib
import json
import re
from pathlib import Path

from playwright.sync_api import sync_playwright

HERE = Path(__file__).resolve().parent
WEBREF_SHA256 = "f9461c26f87a064fbadb00e9bf4be8be20d97c652d5e63cb70ac9d39f6d155b4"
CHROMIUM = "158.0.8127.0"


SAMPLES = {
    "font": "italic 12px monospace",
    "list-style": "square",
    "list-style-type": "square",
    "-webkit-box-orient": "vertical",
    "-webkit-mask-composite": "xor",
}


def candidates(prop, syntaxes):
    # Accept a candidate only after checking the browser's actual CSSOM result.
    # Simple lengths, colors and numbers make good independent computed-value probes.
    common = ["7px", "rgb(11, 37, 73)", "0.25", "3", "0.5s", "23deg", "center",
              "hidden", "grid", "relative", "solid", "bold", "italic", "monospace",
              "repeat(2, 7px)", "rotate(23deg)", "2px 4px", "7px solid red",
              "12px monospace", "7px 3px red", "10% 25%", "12%", "3 / 5",
              "1 / 2 / 3 / 4", "20px / 30px", "ease-in", "manual", '"ab"',
              "linear-gradient(red, blue)", "blur(2px)", 'url("#satin-probe")', "inset(7px)",
              "path('M 0 0 L 3 7')", "border-box", "padding-box", "reverse", "add", "both",
              "paused", "no-repeat", "multiply", "fixed", "isolate", "exclude", "alpha",
              "'liga' 0", "'wght' 350", "red blue", "rect(0px, 7px, 5px, 0px)",
              "satin-example", "--satin-example",
              "bevel", "vertical", "none", "auto", "normal", "initial"]
    # Include keyword choices from directly referenced grammar productions.
    syntax = prop.get("syntax", "")
    refs = re.findall(r"<([^>]+)>", syntax)
    syntax += " " + " ".join(syntaxes.get(ref.strip("'"), "") for ref in refs)
    syntax = re.sub(r"<[^>]+>", " ", syntax)
    keywords = re.findall(r"(?<![\w-])[a-zA-Z][\w-]*(?![\w(-])", syntax)
    preferred = [SAMPLES[prop["name"]]] if prop["name"] in SAMPLES else []
    return list(dict.fromkeys(preferred + common[:-1] + keywords + ["initial"]))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("webref", type=Path, help="css.json from @webref/css 8.7.10")
    parser.add_argument("chromium", type=Path, help="reference Chromium executable")
    args = parser.parse_args()
    source = args.webref.read_bytes()
    if hashlib.sha256(source).hexdigest() != WEBREF_SHA256:
        parser.error("Unexpected Webref data. See README.md before changing the reference.")
    definitions = json.loads(source)
    props = sorted(definitions["properties"], key=lambda p: p["name"])
    syntaxes = {entry["name"]: entry.get("syntax", "") for entry in props}
    syntaxes.update({entry["name"].strip("<>"): entry.get("syntax", "")
                     for entry in definitions["types"]})
    names = [p["name"] for p in props]
    if len(names) != len(set(names)):
        raise ValueError("Duplicate property names in Webref")
    probes = [{"name": p["name"], "values": candidates(p, syntaxes)} for p in props]
    with sync_playwright() as playwright:
        with playwright.chromium.launch(executable_path=args.chromium.resolve(),
                                        args=["--disable-gpu"]) as browser:
            if browser.version != CHROMIUM:
                parser.error(f"Expected Chromium {CHROMIUM}, found {browser.version}")
            page = browser.new_page(viewport={"width": 800, "height": 600}, device_scale_factor=1)
            page.set_content((HERE / "probe.html").read_text())
            rows = page.evaluate("""(probes) => probes.map(({name, values}) => {
                let fallback;
                for (const value of values) {
                    if (!CSS.supports(name, value)) continue;
                    const result = measure(name, value);
                    if (!result.edited) continue;
                    const row = [name, value, result.specified, result.before, result.after];
                    if (result.after !== '' && result.before !== result.after) return row;
                    if (!fallback) fallback = row;
                }
                // CSS-wide keyword permits the declaration probe in Satin even if
                // this reference browser has no implementation for a draft property.
                return fallback || [name, 'initial', null, null, null];
            })""", probes)
    with (HERE / "properties.jsonl").open("w", encoding="utf-8", newline="\n") as output:
        for row in rows:
            output.write(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    main()
