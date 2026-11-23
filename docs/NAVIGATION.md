# Navigation pipeline in alpha.7

The native window and headless mode share one document driver. Navigation
workers deliver the final document address and decoded HTML chunks. The
decoder keeps partial byte sequences between reads. The tokenizer can emit
nodes and request a script, sheet or image before the response body ends.
For HTML without a valid transport charset, the prescan examines up to 1024
bytes. A complete encoding declaration can start decoding early. After the
first chunk, an undecided prefix delays the parser by at most 100 ms. The
decoder keeps the selected encoding for the rest of the document.
Standalone XML SVG has a separate, inert parse path. Data URLs have an
independent bounded decoder for navigation, resources and Fetch.

The parser and realm move the same DOM arena between them. A native callback
never holds the arena or parser borrow while it calls page JavaScript. A
classic script pauses the parser, sees the parsed prefix and becomes
`document.currentScript`. Nested inline scripts inserted by `document.write`
run synchronously. A nested external script leaves its insertion pending until
the outer execution returns. The parser then continues in input order. See
[HTML_STREAMING.md](HTML_STREAMING.md) for tokenizer and tree limits.
See [WebAssembly compilation policy](WASM_CSP.md) for the ordering of HTTP
and parser-delivered CSP and inheritance in classic Blob workers.

External parser resources use bounded worker queues and are associated with
the current tab navigation. Normal classic scripts wait for the relevant
preceding stylesheets; async scripts can run when their source is available.
Classic defer and default module scripts preserve parser order after EOF.
Queued resources have a bounded waiting period. A resource starts its transfer
deadline when it is dispatched. Backpressure keeps the original queue deadline
when the worker rejects a submission.
`interactive` precedes deferred execution. `DOMContentLoaded` follows the
deferred script start sequence. `complete` and window `load` wait for pending
script fetches and the applicable sheet and image loads. Module evaluation
can continue after a top-level suspension.
Fonts can change layout after `load`. Cached image data is shared by URL
and CORS credentials policy; each parser-discovered element receives its own
completion event. Static documents without author scripts, handlers or form
controls still have no VM.

The module map is per document. Static dependencies share parsed module
records and use the actual response URL after redirects. The module host
resolves exact, scoped and prefix import-map rules and blocks null mappings
and prefix escapes. It checks CORS and JavaScript MIME before decoding the
response as UTF-8. SHA-256/384/512 integrity metadata is verified before
source decoding, including import-map integrity entries. Cross-origin
opaque responses cannot be used to satisfy integrity checks.

The module map survives the document load event. Dynamic `import()` uses the
referencing script or module URL, reuses the same records and returns a promise
for the module namespace after evaluation. Top-level await resumes through the
VM job queue. Rejections settle waiting imports with the original reason.
Element scripts have a single preparation per eligible element. Connected
classic inline scripts can run immediately. Dynamic external scripts default
to async and explicit `async = false` preserves their insertion order. Scripts
created by parsing an HTML fragment remain inert. Fetching a module from
`import()` uses the same CORS and JavaScript MIME checks as element scripts.

## Transport and abort

Navigation, parser resources and Fetch share a Tokio reactor with two I/O
threads and separate, bounded queues. Each active HTTP operation races the
navigation generation and the relevant request cancellation signal against its
deadline. Redirects and reads share a deadline. A stopped or replaced navigation
drops its outstanding socket work and ignores late messages for its generation.
Decoders that have already started can finish within a bounded blocking pool.

Fetch preserves the identity of an `AbortSignal` reason in rejected promises.
Already aborted requests are rejected before dispatch. HTTP Fetch resolves its
promise as soon as response metadata passes redirect and CORS checks. The
response stream takes ownership of the body. Transport reads reserve one of two
32 KiB queue slots before polling the HTTP body again. Body transfers have their
own budget of 64 live transfers and release the slots used to open requests.
The UI receives progress events as bytes or EOF become available. Active reads
carry traced promises and temporary VM roots until completion. Cancelling a
reader or dropping the last stream reference releases the transfer. An abort
after headers reaches pending reads with the same reason. Cloned bodies
propagate cancellation upstream when both readers cancel. Whole-body readers
remain capped at 4 MiB, with a separate 8 MiB limit on any unread tee branch.
`AbortSignal.timeout()` queues an internal timer, and `AbortSignal.any()`
propagates the reason from its first aborted source. Fetch and Blob return byte
streams with default and BYOB readers. The reader detaches the supplied
ArrayBuffer and delivers a view over the transferred bytes. Ready BYOB
descriptors leave the queue before their result promises invoke author code.
WritableStream, TransformStream, asynchronous iteration and piping share the
VM promise queue and propagate closure, errors, and cancellation. Streaming
Fetch uploads remain open. [Streams Standard](https://streams.spec.whatwg.org/)

Window, Document and DOM nodes inherit EventTarget. Listener tables are
allocated on first registration and allow up to 4096 listeners per document
realm. Object listeners, passive handling, once and AbortSignal use the same
logic as standalone targets. Event and CustomEvent dispatch take a snapshot of
the ancestor path before invoking author code. After dispatch, currentTarget
and the composed path are cleared. Specialized event constructors, Shadow DOM
retargeting and full legacy inline handler ordering remain open.
[DOM dispatch](https://dom.spec.whatwg.org/#dispatching-events)

DOM lookups by tag, namespace, class and `name`, along with `children` and
`childNodes`, are live. The VM exposes their current indexed own properties.
The views share bounded result caches and honor mutation and parser revisions.
`querySelectorAll` still produces a static snapshot. The HTMLCollection
`namedItem` method works; named properties such as `collection.someId` are
not implemented yet.

The implementation follows the relevant parts of the
[HTML parsing standard](https://html.spec.whatwg.org/multipage/parsing.html),
[module fetching and import maps](https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script),
[DOM collection rules](https://dom.spec.whatwg.org/#old-style-collections),
[AbortSignal](https://dom.spec.whatwg.org/#interface-AbortSignal),
[Fetch](https://fetch.spec.whatwg.org/) and
[Subresource Integrity](https://w3c.github.io/webappsec-subresource-integrity/).
The local verifiers exercise the same HTTP fixture against Rust and an owned
Chrome instance; standalone WPT/Test262 counts are recorded separately.

Open work includes destructive writes (`document.open`), full mutation support
for resource attributes, CSS/JSON module types and import attributes, full
stylesheet import and media rules, CSP, streaming request uploads, BYOB, stream
iteration and piping, and event target
integration across DOM nodes and windows, XML XHTML documents and process
isolation. HTML, module graphs, resources, timers and decoding have finite
budgets. A declaration encountered after the HTML encoding has been selected
cannot restart the parser in this release.

## Storage and window tasks

Script realms in a browser profile share localStorage through the document's
HTTP(S) origin, including scheme and port. SessionStorage uses a separate store
for each tab and retains origin areas across navigation within that tab.
An opaque or local-file origin raises SecurityError. The profile loads
localStorage by origin on first access and atomically saves dirty areas after
pending window work. The storage files preserve UTF-16 code units. Each area
allows up to 5 MiB of strings and 4096 entries. In-memory sessions have a
32 MiB aggregate budget. Events queue in writing order for other windows
sharing the affected area. [Web Storage Standard](https://html.spec.whatwg.org/multipage/webstorage.html)

Writable named properties follow Web IDL. Assigning to a name such as
`localStorage.getItem` writes a stored entry, and the inherited method
still controls the visible property. The tested Chrome 157 creates an
ordinary own property in this specific case.
[Web IDL legacy platform objects](https://webidl.spec.whatwg.org/#legacy-platform-objects)

Timeouts and intervals share cancellable window handles and execute tasks with
a microtask checkpoint. The window uses monotonic deadlines. Foreground
animation callbacks share a timestamp per batch at up to 60 Hz, using the same
navigation origin as `performance.now()`. The event loop wakes only when
there is pending work. Animation batches still use a software deadline. Display
refresh synchronization, Performance Timeline entries, frame trees and
crash restoration of sessionStorage remain open.
[HTML timers](https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#timers) ·
[Animation frames](https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#animation-frames)
