# Pinned tests for CSS `position: sticky`

This suite contains 19 pages and 52 synchronous subtests. The original pages, shared `sticky-util.js`, and upstream `testharness.js` use the bytes from revision `9e1907a13cbbc338975302097a4bafcbc4d12dc5`. The manifest records source paths, SHA-256 digests, and subtest counts. [Web Platform Tests source](https://github.com/web-platform-tests/wpt/tree/9e1907a13cbbc338975302097a4bafcbc4d12dc5/css/css-position/sticky) · [License](../wpt-url/LICENSE.md)

```powershell
cargo test -p rust-browser-engine --test sticky_wpt -- --nocapture
node tools/verify-sticky-wpt.mjs OUTPUT REA_CLI
```

The Rust test runs each original subtest body and preserves its assertions. It registers DOM identifiers as `Window` properties for two files and passes style text from `setupStickyTest` to the engine's `style.cssText`. Geometry and scrolling use the normal engine.

The reference run executes the same pages with the original `testharness.js` in separate document frames at 800 × 600 CSS pixels. Cases cover physical insets, containing blocks, viewport and nested scrolling, `overflow: hidden`, style and DOM changes, and nested sticky elements. The table case measures geometry around table CSS declarations.
