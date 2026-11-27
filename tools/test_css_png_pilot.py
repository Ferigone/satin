import unittest

from css_png_pilot import REF, SRC, TEST, check_page, verdict


class PngPilotTests(unittest.TestCase):
    image = f'<img src="{SRC}" height="50">'

    def test_mismatch_requires_two_nonuniform_canvases_for_pass(self):
        row = {"test": TEST, "refs": [(REF, "!=")]}
        for a, b, different, expected in (
                (False, False, True, "PASS"), (False, False, False, "FAIL"),
                (True, False, True, "INCONCLUSIVE"), (False, True, True, "INCONCLUSIVE"),
                (True, True, True, "INCONCLUSIVE"), (True, True, False, "FAIL")):
            results = {TEST: {"sha256": "a" * 64, "uniform": a},
                       REF: {"sha256": ("b" if different else "a") * 64, "uniform": b}}
            with self.subTest(a=a, b=b, different=different):
                self.assertEqual(verdict(row, results)["status"], expected)
        results[REF] = {"error": "decode failed"}
        self.assertEqual(verdict(row, results)["status"], "ERROR")

    def test_direct_image(self):
        check_page('<!doctype html><title>image</title>' + self.image)

    def test_rejected_paths(self):
        for src in ("https://other/image.png", "//other/image.png", "/support/checkerboard.png",
                    "../support/checkerboard.png", SRC + "?v", SRC + "#a", SRC + ":stream",
                    "support%2fcheckerboard.png", SRC.replace("/", "\\"), "support/unknown.png"):
            with self.subTest(src=src), self.assertRaises(ValueError):
                check_page(self.image.replace(SRC, src))

    def test_extra_dependencies_and_configuration(self):
        for extra in ('<style>div{background:url(x)}</style>', '<link rel=stylesheet href="x">',
                      '<base href="/">', '<script></script>', '<meta name=flags content=image>',
                      self.image[:-1] + ' src="x">', self.image[:-1] + ' srcset="x 2x">',
                      self.image[:-1] + ' style="background:url(x)">'):
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                check_page(self.image + extra)


if __name__ == "__main__":
    unittest.main()
