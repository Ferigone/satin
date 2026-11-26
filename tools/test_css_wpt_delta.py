import unittest
from collections import Counter
from copy import deepcopy

from css_wpt_delta import compare


def report(*statuses):
    rows = [{"test": "css/a", "refs": [["r", "=="], ["s", "!="]], "status": s}
            for s in statuses]
    return {"manifestSha256": "a" * 64, "viewport": [800, 600],
            "inventory": {"reftest": len(rows)}, "counts": dict(Counter(statuses)),
            "results": rows}


class PixelDelta(unittest.TestCase):
    def test_exact_loss_vs_no_verdict(self):
        a = report("PASS", "PASS", "FAIL", "PASS", "ERROR")
        b = report("FAIL", "ERROR", "ERROR", "INCONCLUSIVE", "ERROR")
        b["results"][1]["reason"] = "render timeout"
        b["results"][2]["reason"] = "renderer exited: render timeout"
        result = compare(a, b, "linux-x64", "linux-x64")
        self.assertEqual([r["occurrence"] for r in result["passToFail"]], [1])
        self.assertEqual([(r["occurrence"], r["after"]) for r in result["newlyUnavailable"]],
                         [(2, "TIMEOUT"), (3, "ERROR"), (4, "INCONCLUSIVE")])
        self.assertEqual(sum(result["transitions"].values()), 5)

    def test_input_contract(self):
        a = report("PASS")
        a.update(cohort="linked WPT Ahem stylesheet",
                 resources={"fonts/Ahem.ttf": "b"*64, "fonts/ahem.css": "c"*64})
        self.assertFalse(compare(a, a, "win", "win")["passToFail"])
        for label in ("manifest", "viewport", "cohort", "assets", "references", "test",
                      "inventory", "platform"):
            b = deepcopy(a)
            if label == "manifest":
                b["manifestSha256"] = "f"*64
            elif label == "viewport":
                b["viewport"].reverse()
            elif label == "cohort":
                del b["cohort"], b["resources"]
            elif label == "assets":
                b["resources"]["fonts/Ahem.ttf"] = "d"*64
            elif label == "references":
                b["results"][0]["refs"].reverse()
            elif label == "test":
                b["results"][0]["test"] += "2"
            elif label == "inventory":
                b["inventory"]["reftest"] += 1
            with self.subTest(label=label), self.assertRaises(ValueError):
                compare(a, b, "win", "linux" if label == "platform" else "win")
