import unittest

from css_png_pilot import SRC, check_page


class PngPilotTests(unittest.TestCase):
    image = f'<img src="{SRC}" height="50">'

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
