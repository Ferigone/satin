# Session history, Location, and the document URL

The tab and active document share session history. `pushState` adds an entry, and `replaceState` updates the current entry. Changes immediately affect `location`, `document.URL`, `document.documentURI`, and resolution of links, forms, and relative Fetch requests. A same-document URL change retains the document, its VM, and fetched resources. The first active `<base href>` retains its resolved URL through changes to the document URL.

Setting `location.hash` updates the URL and can dispatch `popstate` during the same call. `hashchange` runs as a queued task, so previously scheduled microtasks can run first. `history.go`, `back`, and `forward` request traversal through the browser. Same-document traversal restores the state for the destination entry. Native keyboard shortcuts use the same mechanism. `PopStateEvent` and `HashChangeEvent` inherit from `Event` and use the shared DOM event implementation.

History state serialization copies a graph of plain objects and arrays. It preserves cycles, shared references, array holes, `undefined`, special numeric values, and UTF-16 strings. `history.state` returns the same object while its entry is active. Returning to an entry restores its saved state. Functions, symbols, and DOM nodes raise `DataCloneError`. Serializing specialized types such as `Map`, `Set`, `RegExp`, buffers, and dates remains open.

History URLs follow component rewrite rules. A changed scheme, host, port, or credentials is rejected. If a restored document redirects to another domain, the new document receives a separate identity and empty history state. Cross-document `Location` changes use the same transport as link navigation.

A session holds up to 100 entries. State budgets are 512 KiB per entry and 4 MiB per tab. After restart, the session restores tab URLs. Persisted history entries, BFCache, POST body replay, and the Navigation API remain open. This stage supports basic fragment scrolling and saves page position during traversal. More restoration modes and text fragments need further work.

## Tab history in the app

Open the Satin menu with the toolbar button or `Alt+F`, then select **Tab history**. The popup lists up to three Back and three Forward entries around the current position, from earliest to latest. Click a destination or use the arrow keys or `Tab`/`Shift+Tab`, then press `Enter` or `Space`. `Escape` or a click outside closes the popup without navigating. The current entry is marked but not selectable; restoring a tab does not restore its earlier or later entries.

This is a view of the active tab's live Back/Forward stack, not a record of visits from other tabs or earlier launches. A row shows a site name or a generic label, not a full URL. Paths, ports, queries, fragments, and credentials are omitted. Internal pages, local files, addresses with credentials, IP literals, and names such as `localhost` or `*.internal` get generic labels. Navigation opens the original entry in the current tab using the same code as Back/Forward.

[Chrome's shortcuts](https://support.google.com/chrome/answer/157179) distinguish the tab-local Back/Forward menu from `Ctrl+H`, which opens broader browsing history. [Firefox's history library](https://support.mozilla.org/en-US/kb/browsing-history-firefox) also covers broader visits with search and management. Satin's tab popup does not implement that library, persistence, deletion, or sync.

Run `node tools/verify-history-ui.mjs BROWSER.exe OUTPUT` for the Windows native smoke check. It uses a loopback fixture, a fresh isolated profile, the app's mouse and keyboard handlers, and saved window frames. It checks a multi-step Back jump, same-document Forward and state restoration, branch truncation, dismissal, and the empty stack after restarting the profile.

## History API and engine verification

The `about:routes` demo changes views through `pushState` and `popstate`. A Map holds the views, a regular expression reads the fragment, and a MutationObserver updates the tab title. The verifier runs one scenario in headless mode, a native window, and a Chrome reference process through REA. A local server checks requests resolved from updated and frozen `baseURI` values, event ordering, and navigation through `location.replace` followed by `history.go(0)` reload. [History verifier](../tools/verify-history.mjs)

HTML defines the URL, state, and event behavior. [History interface](https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-history-interface) · [Location interface](https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-location-interface)
