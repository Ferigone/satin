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
task’s remaining instructions, heap capacity and available thread CPU time. If it exhausts that share,
the child’s error handler receives the failure and the parser can continue
with the following script. Unsupported engine features also reach the child’s
error handler. Operation-specific limits such as RegExp nesting and maximum
string length also reach that handler while the parent task has resources.
The parent and its children still share one overall task budget.
Window timer tasks allow up to 24 million instruction units. Native click,
focus, keyboard and form input tasks allow up to 32 million. Nested handlers
and the microtasks they queue draw from the same task budget. Both kinds of
tasks allow two seconds of thread CPU time on Windows and Linux. Time waiting
for the operating system to schedule the VM does not consume its CPU allowance. A
separate 30-second wall-clock limit applies at the same cooperative execution
checks. An embedded child reserves one quarter of the remaining CPU allowance
for its caller to report a failed script and continue. Platforms without a thread CPU clock use a two-second wall-clock
deadline. Synchronous child parser scripts
remain bounded to at most the ordinary per-script limit. After a long timer,
the scheduler returns to input and rendering before starting another timer.

Frame coverage is limited to initial blank documents. Frame navigation,
`srcdoc`, sandboxed documents, nested frame loading and the full child
DOM are still open. Realm handles are valid inside the VM that created
them. The Test262 CLI uses the same heap and realm entry mechanism for
`$262.createRealm()`.

The [initial-frame regression tests](../crates/engine/src/script/frames.rs)
check live callbacks, distinct intrinsics, split writes, error delivery and
timer cleanup. See the [legacy website probe](../tools/test262/website.md)
for the public Test262 run.
