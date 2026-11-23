# Initial iframe script realms

[`iframe-script-realms.html`](iframe-script-realms.html) is a reduced browser
fixture, not a Test262 test or a suite result. It exercises the iframe call
path used by the [public ES5.1 runner](https://v8.github.io/test262/website/scripts/sth.js)
without depending on jQuery, XMLHttpRequest, or Test262 data.

Serve `tools/fixtures` on loopback over HTTP. For example, from the repository
root, run `python -m http.server 8000 --bind 127.0.0.1 --directory tools/fixtures`.
In separate terminals, use isolated headless profiles:

```text
rust-browser.exe --headless --url http://127.0.0.1:8000/iframe-script-realms.html --profile target/iframe-fixture/satin --run-for-ms 1700 --dump-dom target/iframe-fixture/satin.html
chrome.exe --headless --disable-gpu --no-first-run --user-data-dir=target/iframe-fixture/chrome --virtual-time-budget=2000 --dump-dom http://127.0.0.1:8000/iframe-script-realms.html
```

Inspect `<pre id="results">` in the dumped DOM. Chromium passes 11/11; Satin
alpha.19 passes 0/11. In Satin, missing iframe browsing contexts are already
observable at `frame.contentWindow` and `window.frames`, before calling
`document.open()`. A failed access check is not evidence of a cross-origin
leak: the fixture also requires the expected denial shape from a real frame.

## Implementation boundaries

- The runner creates an ordinary initial blank frame, synchronously accesses
  its document, and assigns live objects and callback functions to the child
  global. Code written in parts through `writeln()` must execute in the child
  realm, not mutate the parent's intrinsics or globals, and invoke callbacks in
  their original realm. An object passed between the two must retain identity.
- `document.open()` replaces the target document's children in place, not the
  document wrapper or the realm. Its origin check uses the entry document, not
  the callee's own global. Complete inline scripts can run before `close()`;
  closing supplies EOF, not a new realm.
- Initial blank frames inherit the creator's origin unless sandbox flags make
  it opaque. Script and same-origin sandbox permissions are separate:
  `allow-same-origin` alone must not allow parser-inserted script execution.
  An opaque active document cannot be read through `contentDocument` or a
  `WindowProxy`, including a proxy saved before navigation.
- A raw Satin object handle is not transferable between separate VM heaps.
  A same-heap design needs distinct, garbage-collected realm state and correct
  realm switching for calls and host callbacks. Treating the parent global as
  the child's, or serializing callbacks and objects, would not implement this
  contract. Do not expose an iframe shim before the origin checks and realm
  boundary are in place.

See the HTML Standard's [iframe element](https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element),
[WindowProxy exotic object](https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-windowproxy-exotic-object),
and [dynamic markup insertion steps](https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dynamic-markup-insertion).
