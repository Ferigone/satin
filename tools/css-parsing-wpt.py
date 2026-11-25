"""Run local synchronous WPT CSS parsing documents with the upstream testharness.

Each selected document gets a fresh Satin runtime. The upstream HTML, scripts,
assertions and helpers are used as-is. The Rust example appends reporting callbacks
to WPT's vendor reporter and advances the document lifecycle and timer queue.
CSS properties that are entirely unimplemented can still pass negative tests.
"""
import argparse
import concurrent.futures
import hashlib
import json
import re
import subprocess
from collections import Counter
from html.parser import HTMLParser
from pathlib import Path, PurePosixPath

HELPER = re.compile(r"\btest_(?:valid_value|invalid_value|computed_value)\s*\(")
ASYNC = re.compile(r"\b(?:async_test|promise_test|promise_setup|requestAnimationFrame|setTimeout|"
                   r"setInterval|step_timeout|addEventListener|fetch|XMLHttpRequest|Worker|SharedWorker)\s*\(")
SUBTEST = ["PASS", "FAIL", "TIMEOUT", "NOTRUN", "PRECONDITION_FAILED"]
ALLOWED_SCRIPTS = {
    "/resources/testharness.js", "/resources/testharnessreport.js",
    "/css/support/computed-testcommon.js", "/css/support/parsing-testcommon.js",
    "/css/support/inheritance-testcommon.js", "/css/support/shorthand-testcommon.js",
    "/css/support/color-testcommon.js", "/resources/check-layout-th.js",
}


class Document(HTMLParser):
    def __init__(self):
        super().__init__()
        self.tag = ""
        self.scripts = []
        self.inline = []
        self.skip = None

    def handle_starttag(self, tag, attrs):
        self.tag = tag
        attrs = dict((k, v or "") for k, v in attrs)
        if tag == "script":
            if set(attrs) & {"async", "defer", "nomodule"} or attrs.get("type", "").lower() not in {"", "text/javascript", "application/javascript"}:
                self.skip = "special script scheduling or type"
            if "src" in attrs:
                self.scripts.append(attrs["src"])
        elif tag in {"img", "iframe", "base", "object", "embed", "video", "audio", "source", "image", "use"} or "src" in attrs or "srcset" in attrs:
            self.skip = "external content or alternate base URL"
        elif tag == "link" and set(attrs.get("rel", "").lower().split()) & {"stylesheet", "preload", "modulepreload"}:
            self.skip = "external document resource"
        elif tag == "meta" and (attrs.get("name", "").lower() in {"variant", "flags", "viewport"} or "http-equiv" in attrs):
            self.skip = "WPT configuration or variants"
        elif any(k.startswith("on") for k in attrs):
            # The geometry helper can run synchronously from Satin's initial load.
            if not (tag == "body" and set(k for k in attrs if k.startswith("on")) == {"onload"}
                    and re.fullmatch(r"""\s*checkLayout\(\s*['"][^'"]+['"]\s*\)\s*;?\s*""", attrs["onload"])):
                self.skip = "event-driven fixture"
        self.check_style(attrs.get("style", ""))

    def handle_endtag(self, tag):
        self.tag = ""

    def handle_data(self, data):
        if self.tag == "script":
            self.inline.append(data)
        elif self.tag == "style":
            self.check_style(data)

    def check_style(self, css):
        if re.search(r"@(?:import|font-face)\b|\burl\s*\(|\bAhem\b", css, re.I):
            self.skip = "stylesheet depends on resources or fonts"


def exclusion(path, any_sync):
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return "requires other encoding"
    if ".sub." in path.name or path.with_name(path.name + ".headers").exists() or "{{" in text:
        return "requires WPT server substitution or headers"
    doc = Document()
    try:
        doc.feed(text)
        doc.close()
    except (ValueError, AssertionError):
        return "cannot classify document"
    if doc.skip:
        return doc.skip
    scripts = "\n".join(doc.inline)
    if not any_sync and not HELPER.search(scripts):
        return "no property parsing helper calls"
    if ASYNC.search(scripts) or "document.fonts" in scripts:
        return "asynchronous fixture"
    if "/resources/testharness.js" not in doc.scripts or "/resources/testharnessreport.js" not in doc.scripts:
        return "missing WPT harness or reporter"
    if set(doc.scripts) - ALLOWED_SCRIPTS:
        return "requires additional script or resource"
    return None


def execute(path, args):
    try:
        result = subprocess.run([args.runner, args.root, path], capture_output=True,
                                timeout=args.timeout, check=True, text=True)
        data = json.loads(result.stdout)
        rows = data.get("tests", [])
        if "error" in data:
            return {"file": path, "status": "HARNESS_ERROR", "reason": data["error"]}
        # Keep partial results when the harness reported an error or never finished.
        harness = data.get("harness")
        diagnostics = data.get("diagnostics", [])
        complete = bool(harness and harness[0] == 0 and harness[2] == len(rows) and rows and not diagnostics)
        status = "COMPLETE" if complete else ("PARTIAL" if rows else "HARNESS_ERROR")
        tests = [{"name": name, "status": SUBTEST[code], **({"message": msg} if msg else {})}
                 for name, code, msg in rows]
        return {"file": path, "status": status, "harness": harness, "diagnostics": diagnostics, "tests": tests}
    except subprocess.TimeoutExpired:
        return {"file": path, "status": "HARNESS_ERROR", "reason": "runner timeout"}
    except (subprocess.CalledProcessError, OSError, ValueError, KeyError, IndexError, TypeError) as error:
        return {"file": path, "status": "HARNESS_ERROR", "reason": str(error)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="pinned WPT checkout")
    parser.add_argument("runner", type=Path, help="compiled css_parsing_wpt example")
    parser.add_argument("report", type=Path, help="full JSON report")
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--glob", help="checkout-relative glob (default css/**/parsing/**/*.html)")
    source.add_argument("--manifest", type=Path, help="inventory every item in the WPT css testharness manifest")
    parser.add_argument("--any-sync", action="store_true", help="also allow documents without property parsing helper calls")
    parser.add_argument("--reuse-report", type=Path,
                        help="reuse finished documents from the identical runner, helper scripts and HTML")
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--timeout", type=float, default=45)
    args = parser.parse_args()
    if args.jobs < 1 or args.timeout <= 0:
        parser.error("jobs and timeout must be positive")
    args.root = args.root.resolve()
    args.runner = args.runner.resolve()
    if not (args.root / "resources/testharness.js").is_file() or not args.runner.is_file():
        parser.error("missing WPT checkout or runner")
    args.glob = args.glob or "css/**/parsing/**/*.html"
    reports = []
    runnable = []
    if args.manifest:
        paths, reports, manifest_digest = manifest_paths(args.manifest, args.root)
    else:
        paths = sorted(p for p in args.root.glob(args.glob) if p.is_file())
    if not paths and not reports:
        parser.error("no documents selected")
    total = len(paths) + len(reports)
    digest = input_digest(args.root, paths)
    for path in paths:
        relative = path.relative_to(args.root).as_posix()
        reason = exclusion(path, args.any_sync)
        if reason:
            reports.append({"file": relative, "status": "NOT_RUN", "reason": reason})
        else:
            runnable.append(relative)
    runner_digest = hashlib.sha256(args.runner.read_bytes()).hexdigest()
    helpers_digest = input_digest(args.root, [])
    cached = {}
    if args.reuse_report:
        prior = json.loads(args.reuse_report.read_text())
        if prior.get("runnerSha256") != runner_digest or prior.get("helpersSha256") != helpers_digest:
            parser.error("prior report used a different runner or helper scripts")
        cached = {r["file"]: r for r in prior["results"] if r["status"] in {"COMPLETE", "PARTIAL"}}
    pending = []
    for name in runnable:
        sha = hashlib.sha256((args.root / name).read_bytes()).hexdigest()
        if name in cached and cached[name].get("sourceSha256") == sha:
            reports.append(cached[name])
        else:
            pending.append((name, sha))
    print(f"{len(runnable)} selected of {total} entries, {len(pending)} require execution", flush=True)
    def test_one(item):
        path, sha = item
        return {**execute(path, args), "sourceSha256": sha}
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for report in pool.map(test_one, pending):
            reports.append(report)
    report = summarize(reports, digest, None if args.manifest else args.glob, args.any_sync)
    report["runnerSha256"] = runner_digest
    report["helpersSha256"] = helpers_digest
    if args.manifest:
        report["manifestSha256"] = manifest_digest
    args.report.write_text(json.dumps(report, ensure_ascii=True) + "\n")
    print(json.dumps({k: report[k] for k in ("pages", "subtests", "subtestsOnCompletePages")}))


def manifest_paths(manifest_path, root):
    data = manifest_path.read_bytes()
    manifest = json.loads(data)
    if manifest["version"] != 9:
        raise ValueError("expected a version 9 WPT manifest")

    def entries(prefix, node):
        for name, item in node.items():
            path = f"{prefix}/{name}"
            if isinstance(item, dict):
                yield from entries(path, item)
            else:
                yield path, item

    selected = []
    unavailable = []
    for source, (sha, *variants) in entries("css", manifest["items"]["testharness"]["css"]):
        p = PurePosixPath(source)
        if p.is_absolute() or ".." in p.parts:
            raise ValueError(f"unsafe manifest path: {source}")
        for url, metadata in variants:
            path = root / p
            if url:
                reason = "variant or generated test URL"
            elif p.suffix.lower() not in {".htm", ".html"}:
                reason = "requires another document type"
            elif set(metadata) - {"timeout"}:
                reason = "requires WPT infrastructure or runner settings"
            elif not path.is_file():
                reason = "document missing from checkout"
            else:
                contents = path.read_bytes()
                if hashlib.sha1(b"blob %d\0" % len(contents) + contents).hexdigest() != sha:
                    raise ValueError(f"source differs from supplied WPT manifest: {source}")
                selected.append(path)
                continue
            unavailable.append({"file": url or source, "status": "NOT_RUN", "reason": reason})
    return selected, unavailable, hashlib.sha256(data).hexdigest()


def input_digest(root, paths):
    digest = hashlib.sha256()
    for key, path in [(p.relative_to(root).as_posix(), p) for p in paths] + [(s, root / s[1:]) for s in sorted(ALLOWED_SCRIPTS)]:
        digest.update(key.encode() + b"\0" + hashlib.sha256(path.read_bytes()).digest())
    return digest.hexdigest()


def summarize(reports, digest, glob, any_sync):
    pages = Counter(row["status"] for row in reports)
    counts = Counter()
    complete = Counter()
    groups = {name: Counter() for name in ("valid", "invalid", "computed", "other")}
    for row in reports:
        for test in row.get("tests", []):
            status = test["status"]
            counts[status] += 1
            if row["status"] == "COMPLETE":
                complete[status] += 1
            name = test["name"]
            if name.startswith("e.style['") and name.endswith(" should not set the property value"):
                group = "invalid"
            elif name.startswith("e.style['") and name.endswith(" should set the property value"):
                group = "valid"
            elif name.startswith("Property ") and " value '" in name:
                group = "computed"
            else:
                group = "other"
            groups[group][status] += 1
    return {"inputSha256": digest, "glob": glob, "anySync": any_sync,
            "pages": pages, "subtests": counts, "subtestsOnCompletePages": complete,
            "subtestsByHelperNamePattern": groups, "results": sorted(reports, key=lambda r: r["file"])}


if __name__ == "__main__":
    main()
