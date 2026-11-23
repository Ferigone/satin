# Navigation failure and recovery

The desktop fallback previously gave general advice, showed the diagnostic on
one long line, and only linked to the start page. It did not distinguish the
next step from the technical explanation.

The error page now puts **Try again** first, with **Start page** as the
secondary action. The error remains verbatim under **Technical details**; long
lines wrap and text is escaped. The heading does not diagnose the machine as
offline: a refused connection, timeout, and certificate rejection can all
prevent a page from loading.

The primary action is only offered for failed HTTP, HTTPS, and file
navigations made with GET. It uses Satin's existing reload path, including
normal transport and certificate checks. A failed form POST does not offer
that action or resend the body. A new navigation removes the old app-owned
error surface rather than leaving an actionable copy on screen.

## Verify on Windows

```powershell
node .\tools\verify-navigation-error.mjs .\target\debug\rust-browser.exe
# With a baseline executable, also capture the previous error presentation:
node .\tools\verify-navigation-error.mjs .\target\debug\rust-browser.exe C:\path\to\baseline.exe
```

This creates a fresh profile per run under
`target/verification-navigation-error/`. It checks a refused loopback
connection after binding and releasing the port, then checks recovery at a
separate loopback address. The recovery fixture drops the first response and
only serves the document on the next request. The smoke action clicks the
rendered retry target through the app's content hit test, then checks the new
title, exact URL, and two GETs to the same path and query. PNG captures and a
JSON receipt are saved alongside the profiles.

The Rust tests in `crates/browser/src/app/navigation_error.rs` cover literal
diagnostics, long lines at the minimum logical window width, retry hit testing,
and suppression of retry for non-GET requests.

## Next design checks

- Loading already changes the reload icon to Stop and labels the footer. See
  whether users can distinguish a new document waiting on its first response
  from a partially visible page finishing its resources, without faking progress.
- Stopping can retain a partial document; stopping before a document exists
  may leave an empty content area and only a footer message. Give that case a
  clear, tab-local explanation and action, without covering a valid partial page.
- The error surface exposes F5 but neither inline action takes keyboard focus.
  Add visible focus and test keyboard and assistive-technology reachability.
  If typed failure information becomes available, use it for more specific
  advice without hiding the underlying diagnostic.
