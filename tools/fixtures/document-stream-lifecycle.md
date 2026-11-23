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
`load` before `close()` returns. The caller is still on the JavaScript stack:
the microtask created by the first `readystatechange` listener runs only after
both listeners and the caller return. Do not assume those timing observations
also apply when a written external script is still in flight. The navigation
unit test `closing_a_written_document_waits_for_its_pending_classic_script`
controls that case separately.
