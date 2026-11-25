"""Verify the single-session browser callbacks and write a deterministic failure index."""
import argparse
import csv
import gzip
import hashlib
import json
from collections import Counter
from html.parser import HTMLParser
from pathlib import Path


class LoadCounters(HTMLParser):
    def __init__(self):
        super().__init__()
        self.values = {}
        self.current = None

    def handle_starttag(self, tag, attrs):
        if tag == "span":
            name = dict(attrs).get("id")
            if name in ("failedToLoadCounter", "failedToLoadCounter1", "failedToLoadCounterDetails"):
                self.current = name
                self.values[name] = ""

    def handle_data(self, text):
        if self.current is not None:
            self.values[self.current] += text

    def handle_endtag(self, tag):
        if tag == "span":
            self.current = None


def read_tsv(path):
    with gzip.open(path, "rt", encoding="utf-8") as source:
        return list(csv.DictReader(source, delimiter="\t"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path, help="output directory of the alpha.40 controller driver")
    parser.add_argument("output", type=Path, help="destination .jsonl.gz failure index")
    args = parser.parse_args()
    references = Path(__file__).resolve().parents[2] / "docs/compatibility"
    catalog = read_tsv(references / "legacy-test262-alpha38.tsv.gz")
    chromium = read_tsv(references / "legacy-test262-reference-alpha38.tsv.gz")
    with gzip.open(references / "legacy-test262-controller-failures-alpha38.jsonl.gz", "rt", encoding="utf-8") as source:
        baseline = {json.loads(line)[0] for line in source}
    with (args.run / "callback-batches.jsonl").open(encoding="utf-8") as source:
        callbacks = [row for line in source for row in json.loads(line)]
    if len(callbacks) != 16436 or len(catalog) != len(callbacks) or len(chromium) != len(callbacks):
        raise ValueError("The original 16,436 callbacks and both reference indexes are required")

    failures = []
    comparison = Counter()
    limit_errors = Counter()
    resource_messages = (
        "Instruction budget exhausted", "Native work budget exhausted",
        "Nested bytecode depth budget exhausted", "Child script CPU budget exceeded",
        "Expression nesting limit exceeded",
        "Aggregate allocation byte budget exhausted; collect between script tasks",
    )
    for index, (row, original, reference) in enumerate(zip(callbacks, catalog, chromium), 1):
        ordinal, path, status, error = row
        if (ordinal != index or int(original["index"]) != index or int(reference["index"]) != index
                or path != original["path"] or status not in ("pass", "fail")):
            raise ValueError(f"Unexpected callback or reference at Run All index {index}")
        comparison[f"Satin {status}, Chromium {reference['chromium151_selected_browser']}"] += 1
        if status == "fail":
            failures.append(row)
            for message in resource_messages:
                if error == f'Error: Unexpected exception, "Error: {message}" was thrown.':
                    limit_errors[message] += 1
                    break

    failed = {row[0] for row in failures}
    count = len(callbacks)
    passed = count - len(failed)
    prefix_failed = sum(index <= 8276 for index in failed)
    cutoff = (args.run / "checkpoint.txt").read_text(encoding="utf-8").strip().split("|")
    if cutoff != ["8276", str(8276 - prefix_failed), str(prefix_failed), catalog[8275]["path"]]:
        raise ValueError("Callback prefix and recorded 8,276 checkpoint disagree")
    rendered = (args.run / "results.txt").read_text().strip().split("|")
    if len(rendered) != 6 or list(map(int, rendered[:3])) != [count, passed, len(failed)]:
        raise ValueError("Results view and original callbacks disagree")
    counters = LoadCounters()
    counters.feed((args.run / "results.html").read_text(encoding="utf-8"))
    run_load = int(counters.values["failedToLoadCounter1"])
    results_load = int(counters.values["failedToLoadCounter"])
    detail = counters.values.get("failedToLoadCounterDetails")
    if run_load != results_load or rendered[3] != (detail or ""):
        raise ValueError("Failed-to-load counters disagree with the connected DOM snapshot")

    payload = "".join(json.dumps(row, ensure_ascii=True, separators=(",", ":")) + "\n"
                      for row in failures).encode("ascii")
    with args.output.open("wb") as output:
        with gzip.GzipFile(filename="", fileobj=output, mode="wb", compresslevel=9, mtime=0) as gz:
            gz.write(payload)
    print(json.dumps({
        "reported": count, "passed": passed, "failed": len(failed), "failed_to_load": run_load,
        "results_failed_to_load_visible_counter": results_load, "results_optional_load_detail": detail,
        "checkpoint_8276": {"passed": 8276 - prefix_failed, "failed": prefix_failed, "path": cutoff[3]},
        "results_table_rows": int(rendered[4]), "results_source_links": int(rendered[5]),
        "changed_from_alpha38_fail_to_pass": sorted(baseline - failed),
        "new_passes_by_catalog_area": dict(sorted(Counter(
            "/".join(catalog[index - 1]["path"].split("/")[1:3]) for index in baseline - failed
        ).items())),
        "new_passes_by_chromium_result": dict(sorted(Counter(
            chromium[index - 1]["chromium151_selected_browser"] for index in baseline - failed
        ).items())),
        "changed_from_alpha38_pass_to_fail": sorted(failed - baseline),
        "chromium_reference": dict(sorted(comparison.items())),
        "fail_callbacks_reporting_vm_limits": dict(sorted(limit_errors.items())),
        "other_fail_callbacks": len(failures) - sum(limit_errors.values()),
        "uncompressed_failure_index_sha256": hashlib.sha256(payload).hexdigest(),
    }, indent=2))


if __name__ == "__main__":
    main()
