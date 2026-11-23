# Offline, read-only chat delivery

This fixture checks one path from a real browser WebSocket to a DOM row. It uses
the [WHATWG WebSockets API](https://websockets.spec.whatwg.org/#the-websocket-interface)
and [RFC 6455](https://www.rfc-editor.org/rfc/rfc6455#section-4.2.2)
for the upgrade and a single unmasked server text frame. The page starts with
an empty log. Only the server has the authored system message. The browser
must receive a trusted text `MessageEvent`, parse its JSON and append the
message with `textContent`. There is no `send()`, simulated event, or fallback.

This standards-based fixture does not emulate Twitch or YouTube or establish
which transport either site uses. It covers neither public chat nor playback.
It requires no Internet access, site accounts, public content, third-party
packages or DevTools. It binds HTTP and WebSocket to one ephemeral `127.0.0.1`
port. The server sends one application frame per accepted connection and does
not read chat input.

## Reproduce on Windows

Run from the repository root with Node.js and a local Chrome or Satin binary
(tested with Node.js 26.13.0). The Satin command resolves the current installed
binary read-only; a candidate `rust-browser.exe` path also works.

```powershell
$chrome = 'C:\Program Files\Google\Chrome\Application\chrome.exe'
(Get-Item -LiteralPath $chrome).VersionInfo.ProductVersion
node tools/fixtures/chat/run.mjs chrome $chrome

$install = Join-Path $env:LOCALAPPDATA 'RustBrowser'
$manifest = Get-Content -LiteralPath (Join-Path $install 'current.json') -Raw | ConvertFrom-Json
node tools/fixtures/chat/run.mjs satin (Join-Path $install $manifest.executable)
```

Each run creates a fresh profile, `dom.html` and `report.json` in its own
`target/chat-fixture/` subdirectory. `delivered` is a boolean. `firstError` is the
earliest page-stage error or `null`; `harnessError` separately identifies an
execution or receipt problem. A run exits `0` for delivery and `1` otherwise.
The transport counters distinguish reaching the page, attempting the upgrade,
accepting it, and writing the frame. The DOM receipt independently checks what
the browser received. After the first failure, later `false` checks mean “not
reached”, not “independently unsupported”. Satin reports its own version;
Chrome's exact version on Windows is the file version printed above, not the
reduced user-agent version.

## Observed headless results

| Browser (Windows x64) | Delivered | First error | Upgrades attempted / accepted / text frames written |
| --- | --- | --- | --- |
| Chrome 157.0.8103.79 | `true` | `null` | 1 / 1 / 1 |
| Installed Satin 0.1.0-alpha.17 | `false` | `constructor: WebSocket unavailable (typeof undefined)` | 0 / 0 / 0 |

Both loaded loopback HTTP, exposed `location.protocol` and `location.host`,
parsed a control JSON object, appended a detached DOM control, and exposed
`MessageEvent`. This does not establish that Satin can dispatch a trusted
WebSocket event, parse a delivered payload or append its live row: all of
those steps were blocked at the missing `WebSocket` constructor. Chrome passed
five consecutive isolated runs. An invocation with a missing binary returned
`harnessError: "ENOENT"` without a page error.

At base `cd96dd68eaa77a4547b711cb5ddac0389c8f3082`, the source also has no
WebSocket API or transport. A future implementation can register a Window
`WebSocket` binding in the [script runtime](../../../crates/engine/src/script.rs)
and route socket lifecycle and received frames into the browser's
[headless](../../../crates/browser/src/headless.rs) and
[native](../../../crates/browser/src/app.rs) event loops. Reuse the existing
[trusted `MessageEvent` machinery](../../../crates/engine/src/script/message_ports/message_event.rs)
at dispatch with the socket's origin, not a synthetic page event or a Fetch
response. This test does not cover `wss:`, auth, subprotocols, reconnects or
production compatibility.
