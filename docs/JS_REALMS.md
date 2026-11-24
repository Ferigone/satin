# JavaScript realms for initial iframes

An attached, unsandboxed initial `about:blank` iframe has its own Window,
Document, global bindings and JavaScript intrinsics. Its parent can reach
the same Window through `contentWindow` and indexed `window.frames`.
The frame document accepts inline classic scripts written through
`open`, `write`, `writeln` and `close`. The parser executes a script
when its closing tag arrives. Uncaught errors reach the child
`window.onerror`, and the tab schedules the child's timers.

The VM shares its heap, task budget, symbol registry and job queue across
these realms. Objects passed between them keep their identity. Function
calls and resumable script bodies enter their defining realm. Removing a
frame or changing its navigation attributes cancels its timers and
releases the host root. JavaScript references to its Window or exported
functions can keep the realm alive.

A script written into a child frame receives a bounded share of the current
task’s remaining instructions and heap capacity. If it exhausts that share,
the child’s error handler receives the failure and the parser can continue
with the following script. Unsupported engine features also reach the child’s
error handler. The parent and its children still share one overall task budget.
A window timer and its microtasks can use up to 24 million instruction units
and have a two-second cooperative deadline. Synchronous child parser scripts
remain bounded to at most the ordinary per-script limit. After a long timer,
the scheduler returns to input and rendering before starting another timer.

Frame coverage is limited to initial blank documents. Frame navigation,
`srcdoc`, sandboxed documents, nested frame loading and the full child
DOM are still open. Realm handles are valid inside the VM that created
them. The Test262 CLI still needs a separate `$262.createRealm()` host
entry point.

The [initial-frame regression tests](../crates/engine/src/script/frames.rs)
check live callbacks, distinct intrinsics, split writes, error delivery and
timer cleanup. See the [legacy website probe](../tools/test262/website.md)
for the public Test262 run.
