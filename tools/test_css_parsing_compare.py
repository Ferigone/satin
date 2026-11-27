"""Lightweight, stdlib-only tests; no runner, browser or WPT checkout needed."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SPEC = importlib.util.spec_from_file_location(
    "css_parsing_compare", Path(__file__).with_name("css-parsing-compare.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def page(file="css/example.html", status="COMPLETE", tests=(("same", "PASS"),)):
    return {"file": file, "status": status,
            "tests": [{"name": name, "status": outcome} for name, outcome in tests]}


def report(*pages):
    return {"inputSha256": "a" * 64, "helpersSha256": "b" * 64,
            "glob": "css/**/parsing/**/*.html", "anySync": False,
            "runnerSha256": "c" * 64, "results": list(pages)}


class CompareTests(unittest.TestCase):
    def test_duplicate_names_and_partial_truncation(self):
        before = report(page(tests=(("same", "PASS"), ("same", "FAIL"),
                                    ("same", "PASS"))))
        after = report(page(status="PARTIAL", tests=(("same", "FAIL"), ("same", "PASS"))))
        result = MODULE.compare(before, after)
        self.assertEqual(
            [(r["occurrence"], r["before"], r["after"]) for r in result["assertions"]],
            [(1, "PASS", "FAIL"), (2, "FAIL", "PASS"), (3, "PASS", "UNOBSERVED")])
        self.assertEqual(result["summary"]["priorPassUnobserved"], 1)
        self.assertEqual(result["summary"]["observedPassLosses"], 1)
        self.assertEqual(result["priorPassUnobserved"][0]["occurrence"], 3)
        self.assertEqual(len(result["partialPages"]), 1)
        self.assertEqual(result["summary"]["assertionTransitionsOnCompletePages"], [])

    def test_occurrence_is_per_name_and_page_not_global_position(self):
        before = report(page(tests=(("a", "PASS"), ("b", "FAIL"), ("a", "FAIL"))),
                        page("css/other.html", tests=(("a", "TIMEOUT"),)))
        after = report(page(tests=(("b", "PASS"), ("a", "PASS"), ("a", "PASS"))),
                       page("css/other.html", tests=(("a", "TIMEOUT"),)))
        result = MODULE.compare(before, after)
        self.assertEqual([(r["assertion"], r["occurrence"], r["before"], r["after"])
                          for r in result["assertions"][:3]],
                         [("a", 1, "PASS", "PASS"), ("a", 2, "FAIL", "PASS"),
                          ("b", 1, "FAIL", "PASS")])
        self.assertEqual(result["summary"]["completeOnBothPages"], 2)

    def test_exact_names_not_normalized(self):
        result = MODULE.compare(report(page(tests=(("same ", "PASS"),))),
                                report(page(tests=(("same", "FAIL"),))))
        self.assertEqual(len(result["assertions"]), 2)
        self.assertEqual(result["summary"]["priorPassUnobserved"], 1)

    def test_error_and_excluded_pages_are_not_assertion_failures(self):
        for status in ("HARNESS_ERROR", "NOT_RUN"):
            with self.subTest(status=status):
                result = MODULE.compare(report(page()),
                                        report(page(status=status, tests=())))
                self.assertEqual(result["assertions"][0]["after"], "UNOBSERVED")
                self.assertEqual(result["summary"]["observedPassLosses"], 0)
                self.assertEqual(len(result["incompletePages"]), 1)
                self.assertEqual(result["partialPages"], [])

    def test_notrun_is_observed_and_new_assertions_are_unobserved_before(self):
        result = MODULE.compare(report(page(status="PARTIAL")),
                                report(page(tests=(("same", "NOTRUN"), ("new", "PASS")))))
        self.assertEqual(result["summary"]["observedPassLosses"], 1)
        self.assertEqual(result["summary"]["priorPassUnobserved"], 0)
        self.assertIn({"before": "UNOBSERVED", "after": "PASS", "count": 1},
                      result["summary"]["assertionTransitions"])

    def test_deterministic_page_order_and_ignored_messages_runner_and_totals(self):
        before = report(page(), page("css/z.html"))
        after = copy.deepcopy(before)
        after["results"].reverse()
        after["runnerSha256"] = "d" * 64
        after["subtests"] = {"PASS": 999}
        after["results"][0]["tests"][0]["message"] = "not part of identity"
        self.assertEqual(MODULE.compare(before, before), MODULE.compare(before, after))

    def test_inventory_must_match_even_for_unrun_pages(self):
        for candidate in (report(), report(page(), page("css/new.html", "NOT_RUN", ()))):
            with self.subTest(candidate=candidate):
                with self.assertRaisesRegex(ValueError, "inventory differs"):
                    MODULE.compare(report(page()), candidate)

    def test_duplicate_page_rejected(self):
        with self.assertRaisesRegex(ValueError, "duplicate page"):
            MODULE.compare(report(page(), page()), report(page()))

    def test_exact_settings_and_manifest_presence(self):
        before = report(page())
        for key, value in (("inputSha256", "d" * 64), ("helpersSha256", "d" * 64),
                           ("glob", None), ("anySync", True),
                           ("manifestSha256", "d" * 64)):
            with self.subTest(key=key):
                after = copy.deepcopy(before)
                after[key] = value
                with self.assertRaisesRegex(ValueError, f"incompatible {key}"):
                    MODULE.compare(before, after)
        before["manifestSha256"] = "e" * 64
        self.assertEqual(MODULE.compare(before, before)["settings"]["manifestSha256"],
                         "e" * 64)

    def test_invalid_schema_rejected(self):
        cases = []
        for key, value in (("inputSha256", "invalid"), ("helpersSha256", None),
                           ("anySync", 0), ("glob", []), ("results", {})):
            bad = report(page())
            bad[key] = value
            cases.append(bad)
        bad = report(page())
        del bad["glob"]
        cases.extend([bad, None, report(page(status="UNKNOWN")),
                      report(page(tests=(("x", "UNOBSERVED"),))),
                      report(page(tests=((None, "PASS"),))),
                      report(page(tests=())),
                      report(page(status="NOT_RUN"))])
        for bad in cases:
            with self.subTest(report=bad):
                with self.assertRaises(ValueError):
                    MODULE.compare(bad, report(page()))

    def test_cli_stdout_and_validation_error(self):
        with tempfile.TemporaryDirectory() as directory:
            baseline = Path(directory) / "before.json"
            candidate = Path(directory) / "after.json"
            baseline.write_text(json.dumps(report(page())), encoding="utf-8")
            candidate.write_text(json.dumps(report(page(status="PARTIAL", tests=()))),
                                 encoding="utf-8")
            command = [sys.executable, "-B", str(Path(MODULE.__file__)),
                       "--baseline", str(baseline), "--candidate", str(candidate)]
            result = subprocess.run(command, capture_output=True, text=True, check=True)
            self.assertEqual(json.loads(result.stdout)["summary"]["priorPassUnobserved"], 1)
            self.assertEqual(result.stderr, "")
            again = subprocess.run(command, capture_output=True, text=True, check=True)
            self.assertEqual(result.stdout, again.stdout)
            candidate.write_text("{", encoding="utf-8")
            error = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(error.returncode, 2)
            self.assertEqual(error.stdout, "")
            self.assertNotIn("Traceback", error.stderr)


if __name__ == "__main__":
    unittest.main()
