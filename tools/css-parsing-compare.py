"""Compare CSS testharness JSON reports without running any tests.

Usage: python -B tools/css-parsing-compare.py --baseline before.json --candidate after.json

Identity is (file, exact name, 1-based occurrence of that name in callback order).
Duplicate names on incomplete pages remain ambiguous if an earlier occurrence was
omitted while a later one ran. No matching by message, status or global position
is attempted. Missing results are UNOBSERVED, not WPT NOTRUN or assertion FAIL.
"""
import argparse
from collections import Counter
import json
from pathlib import Path
import re


PAGE_STATUSES = {"COMPLETE", "PARTIAL", "HARNESS_ERROR", "NOT_RUN"}
TEST_STATUSES = {"PASS", "FAIL", "TIMEOUT", "NOTRUN", "PRECONDITION_FAILED"}
SETTINGS = ("inputSha256", "helpersSha256", "glob", "anySync")


def validate(report, label):
    if not isinstance(report, dict):
        raise ValueError(f"{label}: expected a report object")
    for key in SETTINGS:
        if key not in report:
            raise ValueError(f"{label}: missing {key}")
    for key in ("inputSha256", "helpersSha256", "manifestSha256"):
        if key in report and (not isinstance(report[key], str)
                              or not re.fullmatch(r"[0-9a-f]{64}", report[key])):
            raise ValueError(f"{label}: invalid {key}")
    if type(report["anySync"]) is not bool:
        raise ValueError(f"{label}: anySync must be boolean")
    if report["glob"] is not None and not isinstance(report["glob"], str):
        raise ValueError(f"{label}: glob must be a string or null")
    if not isinstance(report.get("results"), list):
        raise ValueError(f"{label}: results must be an array")
    pages = {}
    assertions = {}
    for row in report["results"]:
        if not isinstance(row, dict) or not isinstance(row.get("file"), str) or not row["file"]:
            raise ValueError(f"{label}: invalid page record")
        file = row["file"]
        if file in pages:
            raise ValueError(f"{label}: duplicate page {file!r}")
        status = row.get("status")
        if not isinstance(status, str) or status not in PAGE_STATUSES:
            raise ValueError(f"{label}: invalid page status for {file!r}")
        pages[file] = status
        tests = row.get("tests", [])
        if not isinstance(tests, list):
            raise ValueError(f"{label}: tests must be an array for {file!r}")
        if status == "COMPLETE" and not tests:
            raise ValueError(f"{label}: COMPLETE page has no assertions: {file!r}")
        if status in {"HARNESS_ERROR", "NOT_RUN"} and tests:
            raise ValueError(f"{label}: {status} page contains assertions: {file!r}")
        occurrences = Counter()
        for test in tests:
            if not isinstance(test, dict) or not isinstance(test.get("name"), str):
                raise ValueError(f"{label}: invalid assertion name for {file!r}")
            outcome = test.get("status")
            if not isinstance(outcome, str) or outcome not in TEST_STATUSES:
                raise ValueError(f"{label}: invalid assertion status for {file!r}")
            name = test["name"]
            occurrences[name] += 1
            assertions[file, name, occurrences[name]] = outcome
    return pages, assertions


def transitions(rows):
    counts = Counter((row["before"], row["after"]) for row in rows)
    return [{"before": before, "after": after, "count": count}
            for (before, after), count in sorted(counts.items())]


def compare(baseline, candidate):
    old_pages, old_tests = validate(baseline, "baseline")
    new_pages, new_tests = validate(candidate, "candidate")
    for key in (*SETTINGS, "manifestSha256"):
        # Presence matters too: an absent manifest is not a null manifest.
        if (key in baseline) != (key in candidate) or baseline.get(key) != candidate.get(key):
            raise ValueError(f"incompatible {key}")
    if old_pages.keys() != new_pages.keys():
        missing = sorted(old_pages.keys() - new_pages.keys())
        added = sorted(new_pages.keys() - old_pages.keys())
        raise ValueError(f"page inventory differs: missing={missing!r}, added={added!r}")
    pages = [{"file": file, "before": old_pages[file], "after": new_pages[file]}
             for file in sorted(old_pages)]
    assertions = []
    for file, name, occurrence in sorted(old_tests.keys() | new_tests.keys()):
        key = file, name, occurrence
        assertions.append({
            "file": file, "assertion": name, "occurrence": occurrence,
            "before": old_tests.get(key, "UNOBSERVED"),
            "after": new_tests.get(key, "UNOBSERVED"),
        })
    complete = {file for file in old_pages
                if old_pages[file] == new_pages[file] == "COMPLETE"}
    partial_pages = [row for row in pages if "PARTIAL" in (row["before"], row["after"])]
    incomplete_pages = [row for row in pages
                        if row["before"] != "COMPLETE" or row["after"] != "COMPLETE"]
    lost_observations = [row for row in assertions
                         if row["before"] == "PASS" and row["after"] == "UNOBSERVED"]
    return {
        "settings": {key: baseline[key] for key in (*SETTINGS, "manifestSha256")
                     if key in baseline},
        "summary": {
            "pages": len(pages),
            "assertionIdentities": len(assertions),
            "pageTransitions": transitions(pages),
            "assertionTransitions": transitions(assertions),
            "priorPassUnobserved": len(lost_observations),
            "observedPassLosses": sum(row["before"] == "PASS"
                                     and row["after"] in TEST_STATUSES - {"PASS"}
                                     for row in assertions),
            "completeOnBothPages": len(complete),
            "assertionTransitionsOnCompletePages": transitions(
                [row for row in assertions if row["file"] in complete]),
        },
        "pages": pages,
        "partialPages": partial_pages,
        "incompletePages": incomplete_pages,
        "priorPassUnobserved": lost_observations,
        "assertions": assertions,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    args = parser.parse_args()
    try:
        baseline = json.loads(args.baseline.read_text(encoding="utf-8"))
        candidate = json.loads(args.candidate.read_text(encoding="utf-8"))
        result = compare(baseline, candidate)
    except (OSError, ValueError) as error:
        parser.exit(2, f"{parser.prog}: {error}\n")
    print(json.dumps(result, ensure_ascii=True, sort_keys=True, indent=2))


if __name__ == "__main__":
    main()
