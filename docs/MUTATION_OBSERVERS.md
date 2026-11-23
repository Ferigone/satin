# MutationObserver

Each document realm tracks its own observer registrations and pending
records. The bindings read observation options in Web IDL dictionary order
and apply the DOM defaults for old-value and filter options. Subtree
observers receive one record per mutation even when several registrations
match. Attribute filters match unnamespaced local names.
[DOM MutationObserver](https://dom.spec.whatwg.org/#interface-mutationobserver)
[Web IDL dictionaries](https://webidl.spec.whatwg.org/#idl-dictionaries)

Records use stable node identities, including nodes that have since been
detached. Each record's added and removed nodes are static NodeList
snapshots. The record's getters reuse the same list instances. The engine
roots an observer while it is registered and traces its callback through
the observer wrapper. Disconnect releases the registration roots and
discards queued records.

Removed subtrees retain transient registrations through the next
notification. Taking records leaves the notification and those transient
registrations in place. Re-observing a target removes transients sourced
from its previous registration. Observer notifications share the FIFO VM
microtask queue with Promise reactions and queueMicrotask. Delivery uses
observer creation order, and each observer's records are taken when its
turn begins. A callback can add records for another observer whose turn is
still ahead. Ordinary callback exceptions appear in browser diagnostics,
and delivery continues to the remaining observers.
[DOM queuing and delivery](https://dom.spec.whatwg.org/#mutation-observers)

Streaming HTML records mutations while the parser has its DOM lease and
enqueues delivery after the document returns to the host. Before preparing
a parser script, the host can run the observer checkpoint with the updated
tree. Synchronous document.write records its changes while the calling
script continues. Parser text records follow the tokenizer's text runs.
Attribute additions to an existing html or body element follow token order.
[HTML parser script end tags](https://html.spec.whatwg.org/multipage/parsing.html#parsing-main-incdata)

The realm admits up to 1,024 registered observers, 4,096 ordinary
registrations, 8,192 transient registrations, and 4,096 queued records.
Pending records have a 4 MiB budget. Retained record data and static
NodeLists each have an 8 MiB budget. The host bounds matching and ancestor
visits per checkpoint, reports excess tracking in its diagnostics, and
keeps admitted records available. Shadow DOM, cross-document adoption,
XML processing instruction mutations, and Range mutation methods remain
outside the implemented DOM surface. Callback exception dispatch through
the global ErrorEvent interface remains open.

## Verification

Run `node tools/verify-mutation-observers.mjs BROWSER.exe OUTPUT --chrome-reference`.
The same local streaming page runs through Rust Browser's headless and
hidden native paths and an owned Chrome process through REA. The first
script installs its observer while the server holds the tail. Both
browsers then exercise parser records, script preparation, and DOM
mutation delivery after a click.

Eight byte-pinned WPT specimens cover 32 observer subtests. The verifier
serves their original files after digest checks and supplies the local
test adapter used by the specimens. It reports all subtests and records
the exact Rust executable digest and owned Chrome cleanup.
The two specimens that call the original nested checkRecords helper
can exhaust the VM's execution-depth budget. Their unreported subtests
remain in the total and are classified as resource-limited. Focused Rust
tests and the shared browser fixture cover their record fields, takeRecords,
and the innerHTML and outerHTML mutation paths.
[Pinned WPT files](https://github.com/web-platform-tests/wpt/tree/fa649586f35119af9032312ef21e300e41d1c3b6/dom/nodes)
