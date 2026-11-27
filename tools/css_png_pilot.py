"""Run one separately reported, manifest-pinned static <img> reftest.

Usage: python tools/css_png_pilot.py WPT_ROOT MANIFEST CSS_REFTEST_RENDER NEW_REPORT
The renderer must be built with --local-png support. This one-row pilot is
distinct from both css-wpt.py cohorts; it does not inventory the remaining WPTs.
"""
import argparse
import hashlib
import json
import os
import re
import runpy
import stat
import subprocess
import tempfile
from html.parser import HTMLParser
from pathlib import Path

baseline = runpy.run_path(str(Path(__file__).with_name("css-wpt.py")))
MANIFEST = "9ac259ce07a43bd66304fc6c8a73277afebeff1418cced979fe8d280c247fd28"
DIRECTORY = "css/css-images/"
TEST = DIRECTORY + "image-rendering-mixed-scaled.html"
REF = DIRECTORY + "image-rendering-mixed-scaled-notref.html"
SRC = "support/checkerboard.png"
ASSET = DIRECTORY + SRC
PIN = "c481380771b69a0d0bbc5fa1ac314c28ceeb93e5642ae15f255cd4aa04f332a5"
CONTROL = DIRECTORY + "pilot-control.html"
CONTROL_HTML = f'<!doctype html><img style="position:absolute;left:792px;top:592px" src="{SRC}">'
# An unscaled image clipped by the viewport: black at even (x+y) in
# the bottom-right 8x8 square, white everywhere else. Little-endian RGB words.
CONTROL_HASH = "7f227e2ad90bd2515c0a343c865de048910a6c539195be902574ad7cd5c2963d"


class ImgPage(HTMLParser):
    """Narrow grammar of this pilot. Never exempt other URL-bearing CSS/HTML."""
    image_count = 0

    def handle_starttag(self, tag, attrs):
        fields = dict(attrs)
        if tag == "img":
            if (len(fields) != len(attrs) or fields.keys() - {"src", "height", "style"}
                    or fields.get("src") != SRC or fields.get("height") != "50"
                    or fields.get("style", "") not in ("", "image-rendering: pixelated;")):
                raise ValueError("unsupported image attributes or source")
            self.image_count += 1
        elif tag == "title" and not attrs:
            pass
        elif (tag == "link" and len(attrs) == 2 and fields.keys() == {"rel", "href"}
              and fields["rel"] in {"help", "mismatch"}):
            pass
        else:
            raise ValueError(f"unsupported pilot element or attributes: {tag}")


def check_page(html):
    parser = ImgPage()
    parser.feed(html)
    parser.close()
    if not parser.image_count or "{{" in html:
        raise ValueError("not a static local image document")
    for allow_ahem in (False, True):
        old = baseline["StaticPage"](allow_ahem)
        old.feed(html)
        old.close()
        if not old.reason:
            raise ValueError("overlap with existing classifier")


def plain_path(path):
    # Inspect before resolving: even an in-root junction is not a pilot input.
    for part in (*reversed(path.parents), path):
        s = part.lstat()
        attrs = getattr(s, "st_file_attributes", 0)
        if stat.S_ISLNK(s.st_mode) or attrs & stat.FILE_ATTRIBUTE_REPARSE_POINT:
            raise ValueError(f"redirected path: {part}")


def snapshot(root, manifest):
    root = Path(os.path.abspath(root))
    plain_path(root)
    files = {}
    for path, kind, cap in ((TEST, "reftest", 4096), (REF, "support", 4096), (ASSET, "support", 304)):
        node = manifest["items"][kind]
        for component in path.split("/"):
            node = node[component]
        if path == TEST and node[1:] != [[None, [["/" + REF, "!="]], {}]]:
            raise ValueError("unexpected reftest comparison")
        p = root / path
        plain_path(p)
        if not p.resolve(strict=True).is_relative_to(root.resolve(strict=True)) or not p.is_file():
            raise ValueError("not a contained regular file")
        headers = [root / parent / "__dir__.headers" for parent in Path(path).parents]
        if any(os.path.lexists(h) for h in [*headers, Path(str(p) + ".headers")]):
            raise ValueError("server headers required")
        with p.open("rb") as stream:
            data = stream.read(cap + 1)
        if len(data) > cap or hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest() != node[0]:
            raise ValueError(f"manifest blob mismatch or budget exceeded: {path}")
        if path == ASSET:
            if len(data) != 304 or hashlib.sha256(data).hexdigest() != PIN:
                raise ValueError("unexpected PNG")
        else:
            check_page(data.decode("utf-8"))
        files[path] = data
    return files


def run(args):
    data = args.manifest.read_bytes()
    if hashlib.sha256(data).hexdigest() != MANIFEST:
        raise ValueError("unexpected manifest")
    files = snapshot(args.root, json.loads(data))
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp)
        (root / DIRECTORY).mkdir(parents=True)
        for page in (TEST, REF):
            (root / page).write_bytes(files[page])
        (root / CONTROL).write_text(CONTROL_HTML, encoding="utf-8")
        (root / "binding.png").write_bytes(files[ASSET])
        command = [args.renderer.resolve(), root, "--local-png", "https://web-platform.test/" + ASSET]

        def render(keys):
            proc = subprocess.run(command, input="\n".join(keys) + "\n", check=True, text=True,
                                  capture_output=True, timeout=args.timeout)
            return dict(zip(keys, map(json.loads, proc.stdout.splitlines()), strict=True))

        control = render([CONTROL])[CONTROL]
        if control != {"sha256": CONTROL_HASH, "uniform": False} or control["uniform"] is not False:
            raise ValueError("local PNG pixel control failed")
        row = {"test": TEST, "refs": [(REF, "!=")]}
        results = {}
        try:
            results = render([TEST, REF])
            row.update(verdict(row, results))
        except subprocess.TimeoutExpired:
            row.update(status="TIMEOUT", reason="pinned pair exceeded worker timeout")
        except (OSError, ValueError, subprocess.CalledProcessError) as exc:
            row.update(status="ERROR", reason=str(exc))
        counts = {s: int(row["status"] == s)
                  for s in ("PASS", "FAIL", "ERROR", "TIMEOUT", "SKIP", "INCONCLUSIVE")}
        return {"cohort": "pinned local replaced PNG", "manifestSha256": MANIFEST,
                "viewport": [800, 600], "resources": {ASSET: PIN}, "control": control,
                "screenEntries": 26318, "outsidePilot": 26317, "counts": counts,
                "fingerprints": results, "results": [row]}


def verdict(row, results):
    for page in (TEST, REF):
        r = results[page]
        if not isinstance(r, dict) or ("error" not in r and (
                type(r.get("uniform")) is not bool or not isinstance(r.get("sha256"), str)
                or not re.fullmatch("[0-9a-f]{64}", r["sha256"]))):
            return {"status": "ERROR", "reason": f"invalid worker fingerprint: {page}"}
    outcome = baseline["compare"](row, results)
    if outcome["status"] == "PASS" and any(results[p]["uniform"] for p in (TEST, REF)):
        return {"status": "INCONCLUSIVE", "reason": "mismatch includes a single-color canvas"}
    return outcome


if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    for name in ("root", "manifest", "renderer", "report"):
        cli.add_argument(name, type=Path)
    cli.add_argument("--timeout", type=float, default=30)
    args = cli.parse_args()
    report = run(args)
    with args.report.open("x", encoding="utf-8") as out:
        out.write(json.dumps(report) + "\n")
