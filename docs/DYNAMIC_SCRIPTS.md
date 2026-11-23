# Dynamic scripts and module loading

HTML script elements now prepare when a DOM operation connects them, inserts
direct child text, or sets a connected script's src attribute. DOM mutations
finish before a script can observe the tree. Each candidate's connection is
checked again because an earlier inline script can move or remove other
candidates. A classic inline script runs during the DOM call, and
document.currentScript returns to the surrounding script when it finishes.
[HTML script preparation](https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element)
[DOM insertion steps](https://dom.spec.whatwg.org/#concept-node-insert)

Preparation snapshots the script kind, source or request URL, and scheduling
flags. Changing those properties later cannot rerun a started script. Empty
and unsupported script elements can become eligible after a later mutation.
Fragment-parsed scripts from innerHTML, outerHTML, and insertAdjacentHTML
remain inert through cloning. Scripts from an HTML document's template
contents can prepare when moved into the connected tree. Cloning copies the
already-started flag.

External dynamic scripts run as soon as their resource is ready by default.
Setting async to false before preparation puts them into an ordered queue.
That queue advances after a failed request, and successful scripts can run
after their nodes are detached. Dynamic modules share the document's import
map and module map. External classic and module execution use the same
transport, MIME, CORS, and integrity checks as parser-discovered scripts.
Dynamic script fetching can delay the window load event. DOMContentLoaded
continues to follow parser and deferred-script completion.

A loaded tab keeps the realm and the module map for later work. It releases
the HTML parser after publishing the final parsed DOM. New script requests
have per-resource deadlines, and existing deadlines remain fixed. An idle
tab has no periodic script wake. Stop discards unfinished parser input and
cancels the old request generation while preserving the partial DOM and
existing module identities.

## Module evaluation and import()

The module VM retains a suspended graph across top-level await. A script
element's load event can run while its graph is suspended. The navigation
session observes subsequent evaluation results for diagnostics. The Promise
returned by import() settles through the VM's FIFO job queue after graph
evaluation. Successful imports return the graph's cached namespace, and
failed evaluation preserves the original rejection value.

Relative imports use the defining source URL, including final redirected
response URLs and inline script base URLs captured at preparation. Import
maps apply to dynamic specifiers. Module-originated requests inherit the
referring module's credentials mode. Classic-originated import() currently
uses same-origin credentials. Requests and source retention remain bounded by
the document resource, graph, and VM limits.

Each document can prepare up to 192 dynamic script elements and queue up to
192 import() requests. Inline source is capped at 1 MiB per script. The
preparation budget counts 8 MiB of source, URL, and integrity strings over
the document's lifetime. The import request budget counts 2 MiB of strings.
Synchronous inline insertion is capped at six nested scripts.

## Verification

Run the browser executable against two loopback HTTP origins using
`node tools/verify-dynamic-scripts.mjs BROWSER.exe OUTPUT --chrome-reference`.
The verifier opens fresh profiles, exercises the hidden native browser's
internal click path as well as headless navigation, and records the owned
Chrome comparison through REA. Request barriers verify ordering without
depending on response-delay guesses. The report includes the binary's
SHA-256, network requests, execution traces, receipts, and Chrome cleanup.

The verifier also serves eight Web Platform Tests with their original bytes
and verifies all 33 registered subtests through a local adapter for the APIs
they call. Three upstream support scripts and the license are pinned with
the tests. The fixture runner checks SHA-256 before serving them.
[Pinned WPT script tests](https://github.com/web-platform-tests/wpt/tree/fa649586f35119af9032312ef21e300e41d1c3b6/html/semantics/scripting-1/the-script-element)

The DOM Standard runs the parent's children-changed steps after a character
data edit. A runtime test covers editing an empty text node inside a
connected script. Chromium 157 left that script unexecuted in the controlled
probe. The shared browser fixture covers direct child insertion.
[DOM character data replacement](https://dom.spec.whatwg.org/#concept-cd-replace)

Open work includes dynamic import-map registration, import attributes and
non-JavaScript module types, SVG script execution, cross-document adoption,
cross-document stream writes, CSP and Trusted Types, and global ErrorEvent
reporting for script exceptions. Lifecycle and security
coverage is limited to the cases recorded by the verifier.
