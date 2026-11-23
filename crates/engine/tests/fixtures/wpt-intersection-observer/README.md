# Pinned tests for IntersectionObserver

The manifest records source paths and SHA-256 digests for revision `9e1907a13cbbc338975302097a4bafcbc4d12dc5`. Cases cover viewport observation, explicit element or document roots, scrolling, clipping, thresholds, `rootMargin`, zero-area targets, and observer state after `unobserve()` or an early `takeRecords()` call. [Web Platform Tests source](https://github.com/web-platform-tests/wpt/tree/9e1907a13cbbc338975302097a4bafcbc4d12dc5/intersection-observer) · [License](upstream/LICENSE.md)

```powershell
cargo test -p rust-browser-engine --test intersection_observer_wpt -- --nocapture
node tools/verify-intersection-observer-wpt.mjs OUTPUT REA_CLI
```

The Rust suite passes original scripts and `intersection-observer-test-utils.js` to `ScriptRuntime` with an 800 × 600 CSS pixel viewport. `bridge.js` supports registration and assertions, and declared fixture IDs are also available on `Window`. The engine supplies the clock, animation frames, observer queues, and geometry.

Seven pages use `calc(100vh + 100px)` to size a spacer. The suite checks the expected subtest count, a passing status for every assertion, and an empty engine diagnostics log. `cargo test -p rust-browser-engine --test intersection_observer` also checks nested scrolling, viewport resizing, and draining records with `takeRecords()`.

The reference run uses the upstream `testharness.js` in separate, visible same-origin frames, each sharing the parent viewport's 800 × 600 CSS pixel dimensions. A reporter collects subtest statuses.
