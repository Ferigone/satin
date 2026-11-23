# Desktop application stage verification

This verifies the application changes separately from the alpha.18 release.
The executable was built for Windows x64 with Rust 1.99.1 from application
source `50293f34829bbe3c4dfa94bd5c8f179cec597d22`, with prebuilt engine and
dependency libraries. It is a native smoke build, not a Cargo release build
or proof of the integrated alpha.18 engine. No desktop shortcuts or installed
profiles were changed.

| Case | Native result |
| --- | --- |
| Startup settings | Six process launches checked the existing default, keyboard and mouse toggles, dismissing the panel, an explicit URL, and the saved behavior after restart. |
| Navigation failure | A refused loopback connection presented the error. A separate request recovered after clicking **Try again**; exactly two GETs reached the same path and query. |
| Crowded tabs | The 640 × 400 case with 24 saved tabs selected the narrow first tab without closing it, then exercised keyboard traversal, the active close target, and opening a new tab. |
| Address editing | Replacing a two-character Unicode selection preserved the surrounding address and navigated to the expected page. The selection covered 440 pixels; the unselected frame had no selection fill, and its caret was 22 pixels high. |
| Start page | At 1586 × 992, the stage changed 385 pixels versus installed alpha.17, all inside the revised toolbar glyphs. No content pixels changed. The installed baseline and stage both differ from the approved design image; this is a no-regression check, not a claim of pixel equality with that image. |

The standalone Rust tests for the toolbar glyphs, tab geometry, and address
editing also passed (10 tests). Native checks ran with throwaway profiles and
in-process smoke actions, not desktop mouse or keyboard automation.
