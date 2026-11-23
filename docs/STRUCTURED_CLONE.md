# Structured cloning

The document realm exposes `window.structuredClone(value, options)`. The binding reads the transfer dictionary and converts its iterable to a list before visiting the value. Uncloneable inputs produce the realm’s `DOMException` with the name `DataCloneError` and code 25. A property getter or an iterator can still throw its original exception. [HTML structured cloning API](https://html.spec.whatwg.org/multipage/structured-data.html#structured-cloning-api)

The VM walks the graph in depth-first order. It records each object once, preserving cycles and shared references. Arrays keep their length and holes. Plain copies keep enumerable own string keys, including UTF-16 code units and `__proto__`, and receive ordinary writable data properties. The walk snapshots the keys before invoking getters and skips any key that a getter deletes. Map and Set snapshots use internal insertion order and preserve aliases across keys, values and other objects. [Structured serialization](https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal)

The clone also supports BigInt, boxed primitive strings, numbers, Booleans
and BigInts, Date, RegExp, built-in errors, ArrayBuffer, DataView and numeric
and BigInt typed arrays. It uses the receiving realm’s intrinsic prototypes.
RegExp copies start with `lastIndex` zero. Built-in error copies retain the
normalized name, own data message and own data cause. All cloned views of the
same buffer share one backing store. Resizable buffers retain their maximum
and views retain their offset and fixed-length or length-tracking mode.

Blob and File clones share immutable backing bytes and preserve their slices
and MIME types. File clones preserve the name and modification time.
DOMException clones read the intrinsic name and message and derive the
legacy code from the name. The message retains individual UTF-16 code units.
[File API](https://w3c.github.io/FileAPI/#blob-section) ·
[DOMException serialization](https://webidl.spec.whatwg.org/#idl-DOMException)

FileList copies retain the ordered Files through the same graph. A file also
present as a separate member in the input refers to the same cloned File as
the corresponding entry in the cloned list. [FileList serialization](https://w3c.github.io/FileAPI/#filelist-section)

ImageData copies preserve dimensions, color space, pixel format and the
typed-array data through the same structured-clone graph. A separately
cloned member that references the data view still references that same
view in the result, including when its backing buffer is transferred.
Chrome 157 copied pixel values in the checked scene and allocated a
separate ImageData view. It also returned null for the checked DOMException
with an unpaired UTF-16 code unit.
[ImageData serialization](https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#imagedata)

ImageBitmap copies have independent wrapper lifetimes and share immutable
pixel storage. Clone and transfer require an attached, origin-clean source.
The receiving wrapper keeps the dimensions and pixels. Transferring or
closing a bitmap clears the dimensions on the original wrapper. A canvas
draw from a tainted bitmap propagates the taint to the destination.
[ImageBitmap serialization and transfer](https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#imagebitmap)

The transfer list accepts attached ArrayBuffers, MessagePorts and
origin-clean ImageBitmaps. The VM validates duplicate entries and transfer
kinds before walking the graph. It completes graph allocation and rechecks
every original before moving any store. The move detaches originals without
running JavaScript or allocating memory. If a getter detaches a buffer or
closes a port or bitmap, that action takes effect and the clone rejects
the list. A transfer can detach a listed original even when the result
graph has no reference to it. The operation uses existing VM instruction
and heap budgets, plus a transfer-list limit of 4,096 objects. Graph traversal
uses a heap-backed stack.
[Transfer serialization](https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializewithtransfer) ·
[MessagePort delivery](MESSAGE_PORTS.md)

Functions, Symbols, weak collections, Promises, module namespace objects,
iterator objects and unsupported platform objects cannot be cloned.
Stream and OffscreenCanvas transfer, SharedArrayBuffer and separate worker
realms remain open. Backtraces and AggregateError-specific payloads are
outside the current error format. The history state serializer still
applies its own persistent storage type set and quota.

## Verification

The comparison fixture serves identical HTML and JavaScript to the Rust browser and an owned headless Chrome. It checks document binding, graph identity, getter order, built-in slots, view aliasing, transfer, and error timing. The report includes both browser receipts and the SHA-256 of the Rust executable.

```powershell
node tools/verify-structured-clone.mjs target/release/rust-browser.exe target/structured-clone-verification both
node tools/verify-structured-clone.mjs target/release/rust-browser.exe target/structured-clone-platform both RELEASE_SHA256 platform
node tools/verify-structured-clone-wpt.mjs target/release/rust-browser.exe target/structured-clone-wpt both RELEASE_SHA256
node tools/verify-image-bitmap.mjs target/release/rust-browser.exe target/image-bitmap-verification both RELEASE_SHA256
node tools/verify-web-streams.mjs target/release/rust-browser.exe target/structured-clone-streams both readable-byte-streams/read-min.any.js
```

The unchanged Streams WPT suite exercises transfer of a BYOB backing
buffer in a real document. The structured-clone comparator serves the
unchanged Window entry script, both batteries and the declared helper
files. It checks their Git blob hashes, records every case and compares
case names across browsers. Set `REA_CLI` to the installed REA CLI script
for the Chrome reference.
[Pinned Streams suite](https://github.com/web-platform-tests/wpt/blob/fa649586f35119af9032312ef21e300e41d1c3b6/streams/readable-byte-streams/read-min.any.js) ·
[Pinned structured-clone entry](https://github.com/web-platform-tests/wpt/blob/fa649586f35119af9032312ef21e300e41d1c3b6/html/webappapis/structured-clone/structured-clone.any.js)
