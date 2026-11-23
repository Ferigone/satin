# WPT Canvas pixel manipulation

The HTML fixtures and `canvas-tests.js` are unmodified from [web-platform-tests](https://github.com/web-platform-tests/wpt/tree/9e1907a13cbbc338975302097a4bafcbc4d12dc5). The manifest records each upstream path and SHA-256. The upstream license is included.

`canvas_pixels.rs` runs the inline scripts in fresh browser realms with the existing assertion bridge and `bridge.js`. The latter schedules the generated synchronous Canvas checks after the fixture DOM exists. Fixture images specified as visual references are unnecessary for these pixel-readback assertions.

The test for floating-point Canvas backing stores is an expected failure. Contexts use an sRGB RGBA8 backing bitmap. Float16 ImageData works independently of the backing format.
