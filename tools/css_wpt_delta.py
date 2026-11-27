"""Compare two original css-wpt.py reports. Platform labels are caller attestations:
the runner does not record the platform, renderer identity or pixel hashes in its report.
"""

import argparse
import json
import re
from collections import Counter
from pathlib import Path


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(value):
    return isinstance(value, str) and re.fullmatch("[0-9a-f]{64}", value)


def index(report):
    require(digest(report["manifestSha256"]), "invalid manifest hash")
    require(report["viewport"] == [800, 600], "unexpected viewport")
    if "cohort" in report or "resources" in report:
        require(report.get("cohort") == "linked WPT Ahem stylesheet", "unknown cohort")
        assets = report.get("resources", {})
        require(set(assets) == {"fonts/Ahem.ttf", "fonts/ahem.css"} and
                all(digest(v) for v in assets.values()), "invalid Ahem resources")
    seen = Counter()
    rows = {}
    for row in report["results"]:
        test, status = row["test"], row["status"]
        require(isinstance(test, str) and test, "missing test identity")
        require(status in {"PASS", "FAIL", "ERROR", "INCONCLUSIVE", "SKIP"}, "unknown status")
        refs = row.get("refs")
        if status == "SKIP":
            require("refs" not in row, "unexpected SKIP references")
        else:
            require(isinstance(refs, list) and refs and all(
                isinstance(r, list) and len(r) == 2 and isinstance(r[0], str) and
                r[0] and r[1] in ("==", "!=") for r in refs), f"invalid references: {test}")
        seen[test] += 1
        rows[test, seen[test]] = row
    require(report["inventory"]["reftest"] == len(rows), "incomplete reftest inventory")
    require(dict(Counter(r["status"] for r in rows.values())) == report["counts"],
            "counts disagree with results")
    return rows


def status(row):
    if row["status"] == "ERROR" and row.get("reason") == "render timeout":
        return "TIMEOUT"
    return row["status"]


def compare(before, after, before_platform, after_platform):
    require(before_platform.strip() and before_platform == after_platform, "platform mismatch")
    old, new = index(before), index(after)
    for field in ("manifestSha256", "viewport", "cohort", "resources", "inventory"):
        require(before.get(field) == after.get(field), f"{field} mismatch")
    require(old.keys() == new.keys(), "test inventory mismatch")
    losses, unavailable, transitions = [], [], Counter()
    for key, a in old.items():
        b = new[key]
        require(a.get("refs") == b.get("refs"), f"ordered references/eligibility mismatch: {key}")
        x, y = status(a), status(b)
        transitions[f"{x}->{y}"] += 1
        record = {"test": key[0], "occurrence": key[1], "before": x, "after": y,
                  "refs": a.get("refs"), "beforeReason": a.get("reason"),
                  "afterReason": b.get("reason")}
        if x == "PASS" and y == "FAIL":
            losses.append(record)
        elif x in ("PASS", "FAIL") and y not in ("PASS", "FAIL"):
            unavailable.append(record)
    return {"platformAttestation": before_platform, "manifestSha256": before["manifestSha256"],
            "cohort": before.get("cohort", "static"), "viewport": before["viewport"],
            "resources": before.get("resources", {}), "transitions": dict(transitions),
            "passToFail": losses, "newlyUnavailable": unavailable}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for side in ("before", "after"):
        parser.add_argument(side, type=Path)
        parser.add_argument(f"--{side}-platform", required=True)
    args = parser.parse_args()
    try:
        result = compare(*(json.loads(p.read_text(encoding="utf-8")) for p in
                           (args.before, args.after)), args.before_platform, args.after_platform)
    except (OSError, KeyError, TypeError, ValueError) as exc:
        parser.error(str(exc))
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
