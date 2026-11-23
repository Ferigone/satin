# Script-created document streams

This control uses a timer after the initial document has loaded. It calls
`open()`, writes a tag, character reference, and classic script in separate
pieces, and closes the new stream. It needs neither frames nor a network
request. The replacement title records the original Document identity, token
boundaries, script execution, readiness, and the `close()` return boundary.
A `bad-open`, `bad-tag`, or `bad-entity` marker means that assertion failed.

Use a fresh, local test profile for each browser. Run from the repository root:

```powershell
$fixture = [Uri]::new((Resolve-Path tools/fixtures/document-stream-lifecycle.html).Path).AbsoluteUri
& $satin --headless --url $fixture --profile "$PWD/target/document-stream-satin-profile" --dump-dom "$PWD/target/document-stream-satin.html" --run-for-ms 550
& $chrome --headless --no-first-run --disable-gpu "--user-data-dir=$PWD/target/document-stream-chrome-profile" --virtual-time-budget=500 --dump-dom $fixture
```

The active Satin preview `484ed9a` and the XHR-only preview `18b9b09`
stop at `open:undefined` with `TypeError: Value is not callable`. Chrome
157.0.8103.79 and Edge 158.0.4370.33 record:

```text
open:function|opened:loading|split-tag|split-entity|before-end|script:loading:true|after-end:true|ready:interactive|second-ready:interactive|dcl:interactive|ready:complete|second-ready:complete|load:complete:true|close-return:complete|ready-job
```

Use the same control on a Satin candidate with script-created document streams.
In this no-resource case both references dispatch `DOMContentLoaded` and Window
`load` before `close()` returns, without a checkpoint in between. This is a
record of the browsers' behavior, not a conformance expectation. HTML's
[`close()` steps](https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#closing-the-input-stream)
finish the tokenizer up to EOF or a spin of the event loop. The
[stop-parsing algorithm](https://html.spec.whatwg.org/multipage/parsing.html#the-end)
sets `interactive` at EOF, runs deferred scripts, and queues separate DOM
manipulation tasks for `DOMContentLoaded` and for `complete`/Window `load`
after load blockers clear. Neither task should be dispatched directly by the
native `close()` call. A [spin of the event loop](https://html.spec.whatwg.org/multipage/webappapis.html#spin-the-event-loop)
is not equivalent to that direct dispatch: it includes a microtask checkpoint.

## Deferred and reentrant controls

Run `document-stream-close-boundaries.html?defer` and
`document-stream-close-boundaries.html?nested` with the same headless
commands and fresh profiles. On both references, `?defer` lets an immediately
available data-URL `defer` script run at `interactive`, before
`DOMContentLoaded`, and emits one `load`.

For a pending-network reference, serve the fixture over HTTP with
`?held-defer`. Serve `held.js` from the same origin after 750 ms, with body
`trace.push('deferred:' + document.readyState);`. Both measured references
returned with `readyState === "complete"` before running `held.js`, then
dispatched `DOMContentLoaded` followed by Window `load`. That early `complete`
is an additional browser observation, not the stop-parsing order above.
The engine regression `written_deferred_script_delays_readiness_events_but_not_the_close_caller`
keeps an external deferred script pending until the test supplies its
response; it expects `interactive` at close return, then deferred script,
`DOMContentLoaded`, and `complete`/Window `load`. The
`closing_a_written_document_waits_for_its_pending_classic_script`
regression instead holds a parser-blocking external script and tests that
readiness remains `loading` until it settles.

With `?nested`, both references keep the same parser script active after its
nested `open()`, keep readiness `loading` when its nested `close()` returns,
and accept its nested write before the surrounding markup resumes. They
finish the unblocked document before the outer write returns. The engine
regression `nested_close_does_not_end_the_active_script_insertion_point`
checks insertion order and one readiness/load cycle across repeated closes,
without requiring the references' synchronous event dispatch.
