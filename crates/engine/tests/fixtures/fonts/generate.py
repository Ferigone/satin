"""Regenerate the project-owned synthetic font fixtures using FontTools[woff]."""
from pathlib import Path
from tempfile import TemporaryDirectory

from fontTools.designspaceLib import AxisDescriptor, DesignSpaceDocument, SourceDescriptor
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont
from fontTools.varLib import build

OUT = Path(__file__).resolve().parent
STAMP = 3800000000


def rectangle(x, y, width, height):
    pen = TTGlyphPen(None)
    if width and height:
        pen.moveTo((x, y))
        pen.lineTo((x + width, y))
        pen.lineTo((x + width, y + height))
        pen.lineTo((x, y + height))
        pen.closePath()
    return pen.glyph()


def build_font(path, family, advance, weight, accents=False):
    builder = FontBuilder(1000, isTTF=True)
    cmap = {0x20: "space", 0x41: "A"}
    if accents:
        cmap.update({0x42: "B", 0x301: "acute"})
    glyphs = {
        ".notdef": rectangle(50, 0, 400, 500),
        "space": rectangle(0, 0, 0, 0),
        "A": rectangle(0, 0, advance - 60, 600),
    }
    widths = {".notdef": (500, 50), "space": (250, 0), "A": (advance, 0)}
    if accents:
        glyphs.update({"B": rectangle(20, 0, 200, 400), "acute": rectangle(60, 640, 60, 120)})
        widths.update({"B": (250, 20), "acute": (0, 60)})
    builder.setupGlyphOrder(list(glyphs))
    builder.setupCharacterMap(cmap)
    builder.setupGlyf(glyphs)
    builder.setupHorizontalMetrics(widths)
    builder.setupHorizontalHeader(ascent=800, descent=-200)
    builder.setupNameTable({
        "familyName": family,
        "styleName": "Regular" if weight == 400 else "Bold",
        "uniqueFontIdentifier": family + str(weight),
        "fullName": family + (" Regular" if weight == 400 else " Bold"),
        "psName": family.replace(" ", "") + "-" + str(weight),
        "version": "Version 1.0",
        "licenseDescription": "Synthetic geometry generated for Rust Browser tests.",
    })
    builder.setupOS2(sTypoAscender=800, sTypoDescender=-200,
                     usWinAscent=800, usWinDescent=200, usWeightClass=weight)
    builder.setupPost()
    builder.font.recalcTimestamp = False
    builder.font["head"].created = STAMP
    builder.font["head"].modified = STAMP
    builder.save(path)


def main():
    build_font(OUT / "wide.ttf", "Audit Wide", 800, 400)
    build_font(OUT / "fallback.ttf", "Audit Fallback", 500, 700, accents=True)
    for flavor in ("woff", "woff2"):
        with TTFont(OUT / "wide.ttf", recalcTimestamp=False) as font:
            font.flavor = flavor
            font.save(OUT / ("wide." + flavor))
    with TemporaryDirectory(dir=OUT) as tmp:
        temp = Path(tmp)
        design = DesignSpaceDocument()
        axis = AxisDescriptor()
        axis.name, axis.tag, axis.minimum, axis.default, axis.maximum = "Weight", "wght", 400, 400, 700
        design.addAxis(axis)
        for weight, advance in ((400, 400), (700, 800)):
            path = temp / f"master-{weight}.ttf"
            build_font(path, "Audit Variable", advance, weight)
            master = SourceDescriptor()
            master.path, master.name, master.location = str(path), f"master-{weight}", {"Weight": weight}
            design.addSource(master)
        font, _, _ = build(design)
        font.recalcTimestamp = False
        font.flavor = "woff2"
        font.save(OUT / "variable.woff2")


if __name__ == "__main__":
    main()
