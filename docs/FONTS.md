# Fonts in alpha.6

CSS keeps font families in the author's order and distinguishes quoted names from generic families such as `serif`, `monospace`, and `system-ui`. Weight and style are separate inherited properties. The `font` shorthand also sets size and line height. Matching considers style, weight, and character coverage. When a glyph is missing, the engine tries the next family, then a bounded list of system families for that Unicode range. Fallback uses grapheme clusters to keep common base characters and accents in a single font.

`@font-face` reads `local()` and `url()` sources with `format()` hints, `unicode-range`, style, and weight range. The decoder opens sfnt files with TrueType outlines, including WOFF and WOFF2 containers. Variable font instances use the same `wght`, `ital`, and `slnt` values for shaping and painting. The slanted `slnt` variant uses 14 degrees. `local()` matches installed fonts by family or face name.

Each shaped run keeps its font faces. A glyph refers to its face's index in the run. Separate layout and renderer instances use the same bytes and variation parameters, including when painting form controls. System fonts from a single file share bytes. Text and glyph caches hold weak references to faces, allowing font data to be released when the last page using it closes.

A downloaded font has a 4 MiB limit before decoding and an 8 MiB limit as sfnt. A page can use up to 32 faces and 32 MiB of sfnt data. Validation before decoding checks table sizes, WOFF2 outline data, and stream totals. The sfnt validator limits points, component graph depth and expansion, cycles, and variable-axis work. The system font catalog caps live data at 64 MiB and 128 faces. The shaped text cache has a 4 MiB budget, shaping plans are limited to an estimated 2 MiB, and glyph bitmaps to 16 MiB. Rasterization charges its work budget before expanding outlines and allocating bitmaps.

Tests cover shaping and painting TTF, WOFF, and WOFF2 files, selecting families and character ranges, variable axes, face sharing with the renderer, and release of font data after the last use. The fixtures use rectangular outlines in the repository. With the same fixtures, Chrome 157 measured 32 CSS px for WOFF2, 42 CSS px for fallback to the second family, and 16, 24, and 32 CSS px at variable weights 400, 550, and 700.

This stage covers monochrome outlines and horizontal text. Downloaded collections, CFF and CFF2, complete bidirectional and line-breaking algorithms, color glyphs, font synthesis, all OpenType axes, exact `oblique` angles, the CSS Font Loading API, and `font-display` timing remain open. Pages using system families depend on the fonts present in Windows.

The implementation follows CSS font matching and WOFF container standards. [CSS Fonts Level 4](https://www.w3.org/TR/css-fonts-4/) · [WOFF 1](https://www.w3.org/TR/WOFF/) · [WOFF 2](https://www.w3.org/TR/WOFF2/)
