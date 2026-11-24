# CSSOM measurements, scrolling, and shared layout

The JavaScript host and renderer share the layout, computed styles, and DOM snapshot. A DOM or form control state change advances the revision. The first layout-dependent read builds layout synchronously. Subsequent reads at that revision reuse the cached `Arc`. The window receives the snapshot by reference. Font faces share immutable data, with bounded shaping and glyph caches per instance. The host creates `Fonts` on the first layout request.

## Supported operations

- `getBoundingClientRect` and `getClientRects` return viewport-relative snapshots in CSS pixels, accounting for positioning, transforms, and scrolling. The rectangles keep their unclipped bounds. Elements without a box get an empty list and a zero rectangle. Rectangles implement `toJSON`, and the list implements `item` and iteration.
- `offsetWidth`, `offsetHeight`, `offsetLeft`, `offsetTop`, and `offsetParent` use the untransformed, unscrolled geometry. The `clientWidth`, `clientHeight`, `clientLeft`, and `clientTop` values use client box sizes and border offsets. `scrollWidth` and `scrollHeight` include the content extent even under `overflow: visible` or `clip`.
- `getComputedStyle` exposes a live read-only view of supported size, font, color, margin, padding, border, position, overflow, and transform properties. CSS and camelCase names are supported. `getPropertyValue` also reads custom properties. `setProperty` and `removeProperty` reject writes.
- `scrollTop`, `scrollLeft`, `scroll`, `scrollTo`, `scrollBy`, `window.scrollX`, `window.scrollY`, `pageXOffset`, and `pageYOffset` support instant scrolling. The new position is clamped to the content bounds and saved. Later content growth starts from that clamped position. When a document shrinks, viewport scrolling is clamped and fixed layout is recalculated as needed.
- `scroll` events are coalesced by target and scheduled as tasks. A document scroll bubbles to the window, and an element scroll stays at the element. Viewport changes schedule `resize`. `onscroll`, `onresize`, and `addEventListener` are supported. Native wheel input starts at the hit element and passes any unused delta up the ancestor chain.

Coverage is partial for the DOMRect and CSSStyleProperties interfaces and prototypes, CSS properties and shorthands, and CSSOM serialization. The used line height approximates `line-height: normal`. Some `auto` insets and percentages with indefinite sizes remain incomplete. Pseudo-elements, `scrollIntoView`, programmatic smooth scrolling (`behavior: "smooth"`), right-to-left and vertical directions, and viewport scrolling behavior need further work. SVG geometry uses limited bounds maps, and other DOM collections in this stage use snapshots. [SVG](SVG.md)

## Native wheel and trackpad scrolling

Satin samples pending wheel input at the monitor's refresh interval. A discrete
wheel detent starts moving the page on the first sample and settles within
roughly 100 ms. Reversing direction interrupts the previous tail. Pixel deltas
and fractional line deltas retain their native distance on both axes and gain
no additional animation. On Windows, disabling client-area animations also
makes whole wheel detents immediate.

The gesture follows the scrollable element under the pointer. When an inner
panel reaches an edge, its parent receives the remainder on that axis. Pages
with scroll-dependent geometry can reuse their formatted content between
frames. Sticky and fixed boxes, hit targets and observer geometry are updated
at the new offset. Resizing or editing the document triggers fresh layout.

## Work budgets and painting

A layout read charges the current VM task before copying document data, parsing CSS, or processing resources. That cost includes external stylesheets and work to build native SVG scenes before exposing geometry. Cached reads reuse the results of those operations. The fatal work limit stops execution even inside a `try/catch` loop.

The renderer maintains a matrix and clip stack and applies the inverse matrix when testing rotated clips. Axis-aligned rectangles use fast row fills. The renderer culls invisible text before glyph rasterization and checks the budget for visible glyphs before allocating bitmaps. The document is limited to 64 Mi work units. An SVG paint call has a separate cap of 16 million units, so the last call can exceed the shared limit by up to that amount. A separate counter bounds outer clip tests, including long clip chains. `paint_limited` reports exhaustion. A separate browser UI budget keeps the address bar and tabs available.

CSSOM tests cover geometry, live styles, layout reuse, scrolling, events, and limits. SVG integration tests cover rendering, bounds, hit testing, and links. `tools/verify-graphics.mjs` starts a local server and runs the same synthetic page in headless mode and a hidden native window. The optional Chrome comparison uses REA in a separate process. [Graphics verification script](../tools/verify-graphics.mjs) · [Reference observations](RESEARCH.md)

The underlying geometry and event semantics are defined by CSSOM View. CSSOM defines `getComputedStyle` and resolved values. [CSSOM View](https://drafts.csswg.org/cssom-view/) · [CSSOM](https://drafts.csswg.org/cssom/)
