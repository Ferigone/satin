# Async XMLHttpRequest author deadlines

An author deadline is separate from the fixed safety deadline of the Fetch
transport. A default `timeout` of zero leaves the request on the existing
transport path. Only an actual author deadline dispatches `timeout`; transport
errors still dispatch `error`, and explicit cancellation still dispatches
`abort`. Synchronous Window requests cannot set `timeout` or open with a
nonzero configured value.

The stored unsigned-long value belongs to the XHR object. The active deadline
belongs to the pending request ID. It is the monotonic sampling instant after
`loadstart` plus the current timeout; in-flight changes use that same start,
including disabling the deadline with zero. Discovering an elapsed deadline
latches it and cancels the transport, but does not run author callbacks inside
the setter. An older queued body continuation for that ID is retired.

The closest XHR deadline is included in the existing `ScriptRuntime` wake-up.
Native window tabs already pass that wake to `WaitUntil`; headless already
includes it in the network wait. No author timers, new threads, timer caps,
transport policy, or generic scheduler changes are needed. The next owner turn
marks due requests and dispatches their timeout work through the same bounded
XHR task FIFO and task budget. Incoming head/body notices also check expiry
before delivering author callbacks; a late notice must not overwrite a
latched outcome.

Fetch sets its done flag inside an end-of-body task. Satin does not have a
separate done-flag task: this candidate uses its owner-side terminal XHR task
as the completion boundary, not the producer's socket EOF time. It stops the
deadline before decoding or firing final callbacks. A busy owner cannot
dispatch timeout events mid-task; if it does not re-enter XHR (for example,
via the setter), transport cancellation also waits until the next owner turn.
The host checks elapsed time before accepting more response work. This leaves
the existing mailbox/continuation arbitration in place and does not claim
exact WHATWG task-source ordering at the deadline/EOF boundary.

A timeout clears response metadata and fires `readystatechange` at DONE,
then trusted `timeout` and `loadend` progress events with zero lengths.
Reopening or aborting removes the old pending ID and its queued task so a
later expiry or transport notice cannot affect the replacement.
It does not change ordinary DOM dispatch or listener checkpoints.

See [XHR timeout and request-error steps](https://xhr.spec.whatwg.org/#the-timeout-attribute)
and [Fetch's done-flag task](https://fetch.spec.whatwg.org/#fetch-finale).
The host tests use explicit monotonic owner times (no sleep or actual network):

```powershell
cargo +1.99.1 test --locked --offline -j 1 -p rust-browser-engine --lib script::fetch::xhr::timeout_tests
cargo +1.99.1 test --locked --offline -j 1 -p rust-browser-engine --test xml_http_request
```
