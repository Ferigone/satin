# Pinned Canvas 2D Web Platform Tests

The 31 upstream HTML files contain 45 subtests and retain their original
bytes. The manifest records their source paths and SHA-256 checksums.
[WPT Canvas at 9e1907a](https://github.com/web-platform-tests/wpt/tree/9e1907a13cbbc338975302097a4bafcbc4d12dc5/html/canvas/element)
[Upstream license](../wpt-url/LICENSE.md)

```powershell
cargo test -p rust-browser-engine --test canvas_wpt -- --nocapture
```

Each file runs in its own document realm. The synchronous bridge supplies
test registration and assertions for this selection. Pixel reads, context
properties and exceptions come from the browser. Reference image tags in
these files serve as illustrations for the original test pages. The runner
uses their embedded JavaScript assertions.
