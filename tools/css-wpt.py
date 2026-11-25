"""Inventory WPT css/ and run exact, self-contained HTML reftests with Satin."""
import argparse
import asyncio
import hashlib
import json
import re
import sys
from collections import Counter, defaultdict
from contextlib import suppress
from functools import cache
from html.parser import HTMLParser
from pathlib import Path, PurePosixPath

# A static document cannot execute script, advance animation time, fetch resources,
# perform navigation or honor runner configuration. Check references just as strictly.
EXTERNAL_CSS = re.compile(r"\burl\s*\(|@(?:import|font-face)\b|\bimage-set\s*\(|\bAhem\b", re.I)
TIMED_CSS = re.compile(r"(?:^|[;{\s])(?:animation|transition)(?:-[\w-]+)?\s*:", re.I)


class StaticPage(HTMLParser):
    def __init__(self):
        super().__init__()
        self.reason = None
        self.in_style = False

    def handle_starttag(self, tag, attrs):
        attrs = {k: v or "" for k, v in attrs}
        self.in_style = tag == "style"
        if tag in {"script", "iframe", "object", "embed", "canvas", "video", "audio", "picture",
                   "img", "base", "image", "feimage", "use", "animate", "animatemotion",
                   "animatecolor", "animatetransform", "set"}:
            self.reason = "script, embedded content or resource"
        elif any(k.startswith("on") or k in {"src", "srcset", "background", "poster"} for k in attrs):
            self.reason = "event or resource attribute"
        elif tag == "link" and set(attrs.get("rel", "").lower().split()) - {
                "help", "author", "reviewer", "match", "mismatch", "assert", "license"}:
            self.reason = "external link dependency"
        elif tag == "meta" and (attrs.get("name", "").lower() in {
                "flags", "fuzzy", "variant", "viewport", "reftest-wait", "reftest-pages",
                "reftest-viewport", "viewport-size", "device-pixel-ratio"} or "http-equiv" in attrs):
            self.reason = "document or environment configuration"
        elif "reftest-wait" in attrs.get("class", "").split() or "autofocus" in attrs:
            self.reason = "requires browser events"
        for attribute in ("style", "fill", "stroke", "filter", "clip-path", "mask",
                          "marker-start", "marker-mid", "marker-end"):
            self.check_css(attrs.get(attribute, ""))

    def handle_endtag(self, tag):
        if tag == "style":
            self.in_style = False

    def handle_data(self, data):
        if self.in_style:
            self.check_css(data)

    def check_css(self, value):
        if EXTERNAL_CSS.search(value):
            self.reason = "stylesheet resource or font"
        elif TIMED_CSS.search(value):
            self.reason = "time-dependent CSS"


def leaves(tree, prefix):
    for name, value in tree.items():
        path = f"{prefix}/{name}"
        if isinstance(value, list):
            yield path, value
        else:
            yield from leaves(value, path)


def select_tests(root, manifest):
    kinds = Counter()
    source_hash = {}
    reftests = {}
    for kind, tree in manifest["items"].items():
        for path, (sha, *variants) in leaves(tree.get("css", {}), "css"):
            source_hash[path] = sha
            if kind != "support":
                kinds[kind] += len(variants)
            if kind == "reftest":
                reftests[path] = variants

    @cache
    def exclusion(path):
        p = PurePosixPath(path)
        if (p.suffix.lower() not in {".htm", ".html"} or p.is_absolute() or ".." in p.parts
                or re.search(r"[?#%\\\r\n]", path)):
            return "requires another document type or URL handling"
        if path not in source_hash:
            return "reference outside CSS snapshot"
        if not (root / path).is_file():
            raise FileNotFoundError(f"missing manifest file: {path}")
        data = (root / path).read_bytes()
        if hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest() != source_hash[path]:
            raise ValueError(f"content differs from supplied WPT manifest: {path}")
        try:
            html = data.decode("utf-8")
        except UnicodeDecodeError:
            return "requires encoding sniffing"
        if "{{" in html or ".sub." in path or (root / (path + ".headers")).exists():
            return "requires WPT server or headers"
        parser = StaticPage()
        try:
            parser.feed(html)
            parser.close()
        except (ValueError, AssertionError):
            return "document classification failed"
        return parser.reason

    rows = []
    pages = set()
    for path, variants in sorted(reftests.items()):
        for url, refs, flags in variants:
            row = {"test": url or path}
            reason = None
            if url or set(flags) - {"timeout"} or not refs:
                reason = "variant, fuzzy comparison or special runner settings"
            else:
                reason = exclusion(path)
            references = []
            if not reason:
                for url_ref, relation in refs:
                    ref = url_ref.removeprefix("/")
                    if relation not in {"==", "!="} or not url_ref.startswith("/") or ref in reftests:
                        reason = "non-local or chained reference"
                        break
                    reason = exclusion(ref)
                    if reason:
                        break
                    references.append((ref, relation))
            if reason:
                row.update(status="SKIP", reason=reason)
            else:
                row["refs"] = references
                pages.add(path)
                pages.update(ref for ref, _ in references)
            rows.append(row)
    return kinds, rows, sorted(pages)


async def fingerprints(paths, args):
    work = iter(paths)
    results = {}

    async def worker():
        proc = None
        try:
            for path in work:
                if proc is None:
                    proc = await asyncio.create_subprocess_exec(
                        str(args.renderer.resolve()), str(args.root.resolve()),
                        stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE)
                try:
                    proc.stdin.write((path + "\n").encode())
                    await proc.stdin.drain()
                    line = await asyncio.wait_for(proc.stdout.readline(), args.timeout)
                    if not line:
                        raise EOFError("renderer exited")
                    results[path] = json.loads(line)
                except (TimeoutError, EOFError, OSError, ValueError) as error:
                    results[path] = {"error": str(error) or "render timeout"}
                    with suppress(ProcessLookupError):
                        proc.kill()
                    await proc.wait()
                    proc = None
        finally:
            if proc is not None:
                proc.stdin.close()
                try:
                    await asyncio.wait_for(proc.wait(), args.timeout)
                except TimeoutError:
                    with suppress(ProcessLookupError):
                        proc.kill()
                    await proc.wait()
    await asyncio.gather(*(worker() for _ in range(args.jobs)))
    return results


def compare(row, rendered):
    actual = rendered[row["test"]]
    refs = [(rendered[p], op) for p, op in row["refs"]]
    for result in [actual] + [r for r, _ in refs]:
        if "error" in result:
            return {"status": "ERROR", "reason": result["error"]}
    matches = [r["sha256"] == actual["sha256"] for r, op in refs if op == "=="]
    mismatches = [r["sha256"] != actual["sha256"] for r, op in refs if op == "!="]
    if (matches and not any(matches)) or not all(mismatches):
        return {"status": "FAIL"}
    if matches and actual["uniform"]:
        return {"status": "INCONCLUSIVE", "reason": "matching single-color canvases"}
    return {"status": "PASS"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("renderer", type=Path)
    parser.add_argument("report", type=Path)
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--timeout", type=float, default=30)
    args = parser.parse_args()
    if args.jobs < 1 or args.timeout <= 0:
        parser.error("jobs and timeout must be positive")
    data = args.manifest.read_bytes()
    manifest = json.loads(data)
    if manifest["version"] != 9:
        parser.error("expected a version 9 WPT manifest")
    kinds, rows, paths = select_tests(args.root, manifest)
    print(f"{len(paths)} distinct pages to paint", file=sys.stderr, flush=True)
    rendered = asyncio.run(fingerprints(paths, args))
    by_module = defaultdict(Counter)
    counts = Counter()
    for row in rows:
        if "refs" in row:
            row.update(compare(row, rendered))
        counts[row["status"]] += 1
        by_module[row["test"].lstrip("/").split("/")[1]][row["status"]] += 1
    report = {"manifestSha256": hashlib.sha256(data).hexdigest(), "viewport": [800, 600],
              "inventory": kinds, "counts": counts, "modules": by_module, "results": rows}
    args.report.write_text(json.dumps(report, separators=(",", ":")) + "\n")
    print(json.dumps(counts, sort_keys=True))


if __name__ == "__main__":
    main()
