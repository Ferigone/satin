# Fetch in classic Blob workers

Supported classic Blob workers use the same Fetch request, response, stream,
and WebAssembly streaming bindings as Window. Relative request URLs use the
worker's Blob URL as the base (a relative HTTP path does not resolve against
it); absolute HTTP(S) requests use the creator's origin and cookie context.
The document's transport client, TLS trust, cookie store, credentials rules,
CORS filtering and redirect handling are reused. Worker Fetch does not access
the document's Blob URL registry.

A worker inherits the intersection of enforced response and already-parsed
head-meta policies at creation. `connect-src`, or `default-src` when
`connect-src` is absent, is checked before connecting and on every followed
redirect. This is a conservative CSP source matcher for worker Fetch, not
general CSP enforcement for Window resources. Unsupported source expressions
do not grant access.

The transport posts response headers before EOF and feeds a bounded body
channel; only the worker event loop touches JavaScript. Termination, worker
shutdown and request abort release the transport or discard stale events.
`WebAssembly.instantiateStreaming` consumes the real response body, but the
compiler still waits for a bounded whole-body buffer (the existing 4 MiB
Fetch body-reader limit). It does not validate or compile Wasm incrementally.

The loopback fixture holds a chunked `application/wasm` response open until
the worker observes its headers; after EOF it checks `postMessage(42)`.
This verifies local transport and VM integration only, not playback on any
streaming service.
