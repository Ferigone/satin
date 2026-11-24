# DevTools smoke: Console and Network

For the current six-panel dock and its controls, see the [DevTools guide](DEVTOOLS.md). The Console and Network baseline below also covers capture boundaries and navigation isolation. For an editing check, open a local HTML page, press Ctrl+Shift+C, select an element and change its width using F2. Read the new inline width from the same element in Console. Repeat a Console evaluation after navigating to a page with no scripts to exercise lazy realm creation.


This checks a **separately verified Windows developer candidate**, not the
installed browser or Satin Preview. Do not launch a browser as part of lane
work. On the integration checkout, record the source commit and the candidate
SHA-256 from independent candidate verification before running any browser
steps below. A screenshot or a legacy diagnostics array alone does not prove
that the native dock routed the correct records.

## Start the loopback fixture

From the repository root with Node 22+:

```powershell
node --test tools/fixtures/devtools-smoke/fixture.test.mjs
node tools/fixtures/devtools-smoke/server.mjs
```

The first command checks the authored scripts and HTTP routes without running
Satin. The second binds an ephemeral port on `127.0.0.1` and prints the five test
addresses. No account, proxy, external resource, or real credential is needed.
The server only serves fixed routes and responses. Ctrl+C prints bounded
per-route response counters and counts of matched synthetic inputs, not raw
traffic. A counter confirms what the server handled, not what DevTools saw.

## Verify a live CSS edit through the Console

Start the fixture server above and replace `PORT` with the port it printed. Use a fresh profile. This action sequence selects the fixture’s button in Elements, changes its inline width, reads the selected element back through `$0.style.width` in Console, and checks that the page title became `237px`.

```powershell
$out = Join-Path $PWD ('target/devtools-edit-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $out | Out-Null
& .\target\debug\rust-browser.exe --url 'http://127.0.0.1:PORT/devtools-a' --profile "$out/profile" --width 1280 --height 900 --smoke-window "$out/edit.png" --smoke-actions tools/fixtures/devtools-smoke/edit-style.json
```

## Physical F12 and panel review

Use a fresh PowerShell session. Replace the executable, digest and port below
with the values from independent verification and the fixture. Invoke the
developer executable directly. Do not open or reuse a production profile.

```powershell
$exe = (Resolve-Path '.\target\devtools-candidate\rust-browser.exe').Path
$verifiedSha256 = '<independently verified candidate SHA-256>'
if ((Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash -ne $verifiedSha256) {
    throw 'Candidate digest mismatch; do not launch'
}
$origin = 'http://127.0.0.1:<printed-port>'
$run = New-Item -ItemType Directory -Path (Join-Path $PWD ('target\devtools-manual-' + [Guid]::NewGuid().ToString('N')))
Get-ChildItem Env: | Where-Object Name -Match '^(all|https?|no)_proxy$' | ForEach-Object {
    Remove-Item -LiteralPath "Env:$($_.Name)"
}
$env:NO_PROXY = '127.0.0.1'
$env:LOCALAPPDATA = (New-Item -ItemType Directory -Path (Join-Path $run.FullName 'local-app-data')).FullName
& $exe --profile (Join-Path $run.FullName 'profile') --url "$origin/devtools-a"
```

1. **Toggle.** Wait for **DevTools A ready**. Press physical **F12**: a dock
   with **Console**, **Network**, **Elements**, **Sources**, **Performance** and **Application** opens and the document still renders.
   Press F12 again: it closes and gives the document its space back. Reopen
   with page focus and repeat with the address bar focused. Hold F12 briefly:
   autorepeat should not flap the dock. Resize to a small supported window;
   the dock should not overlap interactive page content. These input checks
   require the real window, not the synthetic action runner.
2. **Console.** Use the unfiltered view (including Debug). On A, find
   `DT-A-LOG`, `DT-A-INFO`, `DT-A-WARN`, `DT-A-ERROR`, and `DT-A-DEBUG`.
   `DT-A-WARN` and `DT-A-ERROR` must be distinguishable. `DT-A-CAUGHT` is
   only an explicit info log, not a reported failure. `DT-A-THROWN` and
   `DT-A-UNHANDLED` are browser-reported failures, distinct from console calls;
   `DT-A-HANDLED-REJECTION` must not appear. The separately thrown and rejected
   object can be labeled generically; `Thrown-object hooks: 0` must stay
   unchanged and `DT-A-HOOK-RAN` must never appear. Check source/phase only
   when the reporter has it. Optional, after the isolation checks on a
   fresh A document: click **Emit 520 debug messages** once. Check clipping
   and scrolling; if the capture limit is exceeded, the discarded-record
   count must be visible. Clear the view and check that old rows are gone.
3. **Network.** On A check the table below. Check the method, category and
   status against the fixture. Time is required only if observed; status at
   headers is not proof that a body completed. A Fetch HTTP 404 is an HTTP
   response, not a transport failure or a rejected Promise.

   | A path | Method | Expected status | Source |
   | --- | --- | --- | --- |
   | `/devtools-a` | GET | 200 | Document |
   | `/devtools-a/fetch-ok` | POST | 200 | Fetch |
   | `/devtools-a/fetch-missing` | GET | 404 | Fetch |
   | `/devtools-a/xhr-ok` | GET | 200 | XHR, if implemented |
   | `/devtools-a/xhr-missing` | GET | 404 | XHR, if implemented |

   The page must show `Fetch: 200/404`. Alpha.19 has no page
   `XMLHttpRequest` binding; record `XHR: unsupported` and no XHR rows or
   server hits, not an XHR pass. If a candidate implements the API, the page
   uses real `XMLHttpRequest` calls and must show `XHR: 200/404`. A combined
   **Fetch/XHR** category is acceptable, but Fetch does not prove XHR.
4. **Isolation.** Open B (`$origin/devtools-b`) in a second tab. Inspect
   both panels after **DevTools B ready**: only `DT-B-*` logs/failures
   and B paths, no A entries. Switch back to A without reloading and check
   that its A entries are still there, without B entries. Follow A's
   **Navigate this tab to a clean document** link in the same tab. Console
   should be empty and Network should contain only the new document's
   `/devtools-clean` request, not A or B. B's own records should still be
   present when you switch back. If switching views or tabs closes the dock,
   reopen it before inspecting.
5. **404 and redaction.** In the clean tab visit the printed **404** URL;
   verify a Document row for `/devtools-missing`, HTTP 404. Then visit the
   printed **Redaction** address literally (fake URL username/password plus
   query and fragment); verify a Document row with the loopback origin and
   `/devtools-redaction`, without userinfo, query or fragment. In that row
   and the earlier Fetch row, inspect all panel text and any URL reveals such
   as tooltips. The address bar is not the DevTools display. No private header,
   cookie, or request/response body should appear in the panel:

   | Input | Must not appear in Network |
   | --- | --- |
   | Navigation URL | `DT-NAV-USER`, `DT-NAV-PASS`, `DT-NAV-QUERY`, `DT-NAV-FRAGMENT` |
   | Fetch URL | `DT-FETCH-QUERY`, `DT-FETCH-FRAGMENT` |
   | Fetch request | `DT-REQUEST-AUTH`, `DT-REQUEST-HEADER`, `DT-REQUEST-BODY` |
   | Fetch response | `DT-RESPONSE-HEADER`, `DT-RESPONSE-COOKIE`, `DT-RESPONSE-BODY` |

   A successful `query` match in the server counters shows the query was
   sent; fragments never go on the wire. Basic Authorization delivery for
   navigation depends on the client; an absent counter does not prove
   redaction. If the source strips userinfo or fragment before the observer
   sees it, mark that input unexercised natively and verify it in the model
   with a URL that actually contains it. Never fill rows for invisible work.

For an unobserved source or status, record exactly what was absent and the
loopback counter or page outcome. In particular, baseline streamed HTML
navigation does not put HTTP status in `NavigationPart::Open`; document
200/404 needs a candidate observation seam that actually sees the headers.
Missing required rows or statuses are coverage gaps, not passes.

## Automated native capture (after verification only)

The runner below uses the existing `--smoke-window` and `--smoke-actions`
contract. Integration must map `"F12"` to `NamedKey::F12` in the synthetic
key decoder; alpha.19 falls through to `Key::Character("F12")`. A receipt
from that decoder without the mapping cannot verify the shortcut. The plans
assume that the dock opens to Console and stays open when switching tabs or
navigating. If a candidate uses different dock state, use the physical steps
or adapt its action sequence before collecting evidence.

```powershell
# $exe and $verifiedSha256 are the independently verified candidate above.
# Supply logical client-area X/Y in the center of the candidate's Network tab
# for the 1280x900 capture window and current Windows scaling.
node tools/fixtures/devtools-smoke/native.mjs $exe $verifiedSha256 $networkX $networkY
```

Omit both coordinates to collect only the Console/toggle images. The runner
uses separate fresh profiles and loopback servers per case, asserts native
frames and titles, checks legacy diagnostics markers, checks Fetch delivery
and counts any XHR calls, then writes PNGs and reduced receipts to
`target/verification-devtools/native-*`. It does not read panel state or
perform OCR. Compare `console-open.png` and
`console-closed.png`, then inspect the A, B, returned-A, clean, 404 and
redaction captures against the checklist; a PNG signature or server count
does not assert a row, severity, or redaction. Physical F12, pointer/focus
routing and tooltip checks are manual.

Keep a local QA record of commit, executable SHA-256, screenshot names,
physical F12 result, row/source coverage, XHR capability, isolation and
redaction. Keep fixture and engine/model test results separate from native
evidence. This is not a website, worker/resource coverage, or telemetry
export test; do not publish captures or reuse the scratch profiles.
