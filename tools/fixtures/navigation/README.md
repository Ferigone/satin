# Navigation verifier fixtures

Run the verifier with the Rust Browser executable and a fresh evidence directory.

```powershell
node tools/verify-navigation.mjs target/debug/rust-browser.exe target/navigation-verification --chrome-reference
```

The fixture uses two loopback HTTP origins. The main URL redirects to a new
directory, and the page sets a relative `base` URL. Classic scripts and modules
also redirect. Module children resolve relative to the final resource URLs.
The request log records origins, methods, paths, CORS Origin headers and response
statuses. The receipt verifies script, `document.write()` and lifecycle order.
An async script response waits for the page's `DOMContentLoaded` request. A
bounded fallback releases it on failure so the verifier can report what happened.

Page observations cover the partial DOM at a parser-blocking script, blocking
CSS, nested scripts and `currentScript`, deferred and async execution, module
imports and live bindings, and a derived class with a field and `super` call.
The receipt also verifies the exact `load` and `error` events for the script
elements. Subresource checks cover valid SRI, a mismatched strongest digest,
cross-origin SRI with CORS, a denied module, and a classic script rejected by
MIME policy. Successful and rejected scripts use separate observable effects.

The HTTP streaming case sends a prefix and withholds the tail until the server
receives the prefix's script request. It then writes the tail in two chunks
that split one UTF-8 character. The early script checks the visible prefix and
registers a `DOMContentLoaded` listener to report the decoded tail. The verifier
requires the early request to release the tail, and saves the exact chunk bytes
and transport event order for each browser run.

The four pinned html5lib scripted cases are read directly from the engine's test
data. The verifier checks each original `.dat` file against its pinned SHA-256,
serves the extracted `#data` bytes, and compares the browser DOM with the expected
`#document` tree. Every requested case has a separate result in `report.json`.

`--rust-headless-only` skips the native window for faster debugging.
`--chrome-reference-only` runs the reference without a Rust executable.
The `--rea`, `--chrome` and `--timeout-ms` options override the default paths
and per-process timeout. REA launches an owned headless Chrome and its result
must confirm cleanup. Rust's native run uses an internal title smoke action
and a fresh profile. The verifier saves the receipts, DOMs, commands, stdout,
stderr, and Rust screenshot hashes in the evidence directory.
The version probe records launch failures before any Rust navigation cases run.
