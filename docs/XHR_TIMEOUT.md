# Async XMLHttpRequest author deadlines

An author deadline is separate from the fixed safety deadline of the Fetch
transport. A default `timeout` of zero leaves the request on the existing
transport path. Only an actual author deadline dispatches `timeout`; transport
errors accepted before expiry still dispatch `error`, and explicit
cancellation still dispatches `abort`. Synchronous Window requests cannot set
`timeout` or open with a nonzero configured value.

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

## EOF/deadline boundary in `dea0a9f`

`D` is the current author deadline. The deciding clock sample is on the owner
thread, not when the producer publishes EOF or when a notice enters the mailbox:

| Terminal work | Owner samples before `D` | Owner samples at `D` or later |
| --- | --- | --- |
| Local queued EOF (`End` for a buffered response; `Read` for a ready pipe) | The selected task can consume EOF and retire the deadline before final callbacks. | The pump latches timeout before popping a task. The old continuation is removed and one timeout goes to the tail. |
| Mailbox progress for a pipe with EOF ready | Reads EOF if no local continuation owns the read; otherwise leaves the local continuation in charge. | Latches or preserves timeout before checking local ownership or reading the pipe. Does not dispatch DONE inline. |
| Mailbox transport error before headers | Delivers `DONE → error → loadend` in the mailbox task and removes the ID. | Suppresses the transport result and queues (or keeps) `DONE → timeout → loadend` for the XHR pump. |

`Host::run_next_task` samples once before its due scan and dequeue;
`Host::progress` and `Host::complete` sample on entry. A selected terminal
read does not take another sample in `finish` to break a tie.

For example, with local queue `[A EOF, B EOF]` and only A expiring, a step
before `D` consumes A's EOF. A step at `D` instead rewrites the queue to
`[B EOF, A Timeout]`; B can complete before A's timeout events even though
A's cancellation is already latched. Mailbox progress for A handled first
at `D` produces the same rewrite. A pump scan orders newly due IDs by
`(deadline, id)` before tail insertion, but does not reorder unaffected work
or already-latched tasks.

There is no unified networking task queue. `FetchTransfer::run` publishes a
terminal result, then sends a progress notice if the pipe and generation are
still live. Neither publication nor queue insertion retires the deadline.
The headless loop can deliver up to 16 mailbox messages before pumping local
continuations; mailbox callbacks for another request can therefore run before
already-queued local work. This is not an order guarantee for competing requests.

Fetch sets its done flag in an end-of-body task, and XHR's timeout wait observes
that flag. Satin has no separate done-flag task and treats its accepted terminal
XHR work as the completion boundary. This captures the current local tie rule,
not exact WHATWG ordering between the done-flag task, incremental body close,
and timeout. A busy owner cannot dispatch timeout events mid-task; if it does
not re-enter XHR, transport cancellation also waits until it yields.

A timeout clears response metadata and fires `readystatechange` at DONE,
then trusted `timeout` and `loadend` progress events with zero lengths.
Reopening or aborting removes the old pending ID and its queued task so a
later expiry or transport notice cannot affect the replacement.
It does not change ordinary DOM dispatch or listener checkpoints.

## Validation

See [XHR timeout and request-error steps](https://xhr.spec.whatwg.org/#the-timeout-attribute)
and [Fetch's done-flag task](https://fetch.spec.whatwg.org/#fetch-finale).
The host tests use explicit monotonic owner times (no sleep or actual network):

```powershell
cargo +1.99.1 test --locked --offline -j 1 -p rust-browser-engine --lib script::fetch::xhr::timeout_tests::deadline_boundary_
cargo +1.99.1 test --locked --offline -j 1 -p rust-browser-engine --lib script::fetch::xhr::timeout_tests
cargo +1.99.1 test --locked --offline -j 1 -p rust-browser-engine --test xml_http_request
```
