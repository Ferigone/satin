# Session history, Location, and the document URL

The tab and active document share session history. `pushState` adds an entry, and `replaceState` updates the current entry. Changes immediately affect `location`, `document.URL`, `document.documentURI`, and resolution of links, forms, and relative Fetch requests. A same-document URL change retains the document, its VM, and fetched resources. The first active `<base href>` retains its resolved URL through changes to the document URL.

Setting `location.hash` updates the URL and can dispatch `popstate` during the same call. `hashchange` runs as a queued task, so previously scheduled microtasks can run first. `history.go`, `back`, and `forward` request traversal through the browser. Same-document traversal restores the state for the destination entry. Native keyboard shortcuts use the same mechanism. `PopStateEvent` and `HashChangeEvent` inherit from `Event` and use the shared DOM event implementation.

History state serialization copies a graph of plain objects and arrays. It preserves cycles, shared references, array holes, `undefined`, special numeric values, and UTF-16 strings. `history.state` returns the same object while its entry is active. Returning to an entry restores its saved state. Functions, symbols, and DOM nodes raise `DataCloneError`. Serializing specialized types such as `Map`, `Set`, `RegExp`, buffers, and dates remains open.

History URLs follow component rewrite rules. A changed scheme, host, port, or credentials is rejected. If a restored document redirects to another domain, the new document receives a separate identity and empty history state. Cross-document `Location` changes use the same transport as link navigation.

A session holds up to 100 entries. State budgets are 512 KiB per entry and 4 MiB per tab. After restart, the session restores tab URLs. Persisted history entries, BFCache, POST body replay, and the Navigation API remain open. This stage supports basic fragment scrolling and saves page position during traversal. More restoration modes and text fragments need further work.

The `about:routes` demo changes views through `pushState` and `popstate`. A Map holds the views, a regular expression reads the fragment, and a MutationObserver updates the tab title. The verifier runs one scenario in headless mode, a native window, and a Chrome reference process through REA. A local server checks requests resolved from updated and frozen `baseURI` values, event ordering, and navigation through `location.replace` followed by `history.go(0)` reload. [History verifier](../tools/verify-history.mjs)

HTML defines the URL, state, and event behavior. [History interface](https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-history-interface) · [Location interface](https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-location-interface)
