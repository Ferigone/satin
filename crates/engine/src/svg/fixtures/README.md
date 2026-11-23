# Pinned SVG shape reftests

The fourteen `.svg` files here are unmodified files from
[`web-platform-tests/wpt`](https://github.com/web-platform-tests/wpt/tree/9e1907a13cbbc338975302097a4bafcbc4d12dc5/svg/shapes),
revision `9e1907a13cbbc338975302097a4bafcbc4d12dc5`. `LICENSE.md` is the
unmodified license from that revision. `manifest.json` records each upstream
path and the SHA-256 of the downloaded bytes. Files were downloaded directly
from `raw.githubusercontent.com`; XML, metadata, styles and references were not
rewritten for this implementation.

`tests/svg_conformance.rs` runs all seven selected source/reference pairs through
the standalone SVG XML front end, the browser's own CSS cascade, scene building
and software rasterizer. It compares every RGB pixel against the corresponding
reference rendered by the same pipeline, and fails on scene/render diagnostics.
There are no skips or unsupported exclusions in this seven-pair denominator.

| Pair | Assertion exercised |
| --- | --- |
| circle-01 | A circle radius of zero disables rendering. |
| ellipse-01 | CSS `rx` and `ry` override missing presentation radii. |
| ellipse-02 | `ry:auto` uses the absolute `rx` length. |
| ellipse-03 | `rx:auto` uses the absolute `ry` length. |
| ellipse-04 | Both auto radii disable rendering. |
| ellipse-07 | A percentage radius resolves in viewBox units before auto copying. |
| line-dasharray | `pathLength` calibrates dash lengths. |

These selected cases establish feature regressions, not SVG-wide WPT
conformance or an independent raster oracle. The full upstream SVG test suite
has not been run. Independent geometric/pixel expectations and resource tests
are in `tests/svg_rendering.rs` and `tests/svg_document.rs`.
