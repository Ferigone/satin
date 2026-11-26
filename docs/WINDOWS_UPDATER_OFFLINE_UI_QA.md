# Quiet Cinder: offline updater UI capture

This QA-only branch also has a separate resize test. Give it a fresh absolute
`SATIN_QA_FAVORITES_OUTPUT` with an existing parent, then run the ignored
`app::updater_ui_qa::capture_favorites_resize_and_scroll_offline` browser unit
test in its own process. It resizes the same Winit window from 1280×800 to
390×800 and back at 100%, sends wheel input through the browser's Winit
handler without injecting OS input, and saves the settled frames. It checks
clipped favorite hits, their invalidation before repaint, Quiet exposing the
page at y=91, scrolling the document from the former bar area, and restoring
the wide geometry while preserving page scroll. It makes no HTTP requests
and does not establish Chromium pixel parity or higher-DPI behavior.

This ignored test runs the real Windows x64 Winit app and presenter. It is
QA-only. Run it **after** the build/run gate on an interactive 100% DPI
desktop with enough space for a 1280×800 client. Do not interact with the
window during the run. The fixture requires exactly 390×800 or 1280×800
client pixels at scale 1. A clamped, scaled, or unmatched client is not a pass.

From the workspace root, run **one width per test process** (Winit only
permits one event loop per process). This creates fresh output directories
under the system temp directory, not under a Satin install or profile.

```powershell
$qaRoot = Join-Path $env:TEMP ("satin-updater-ui-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $qaRoot -ErrorAction Stop | Out-Null
foreach ($width in 390, 1280) {
    $env:SATIN_QA_UPDATER_WIDTH = "$width"
    $env:SATIN_QA_UPDATER_OUTPUT = Join-Path $qaRoot "$width"
    cargo test --locked -p rust-browser --bin rust-browser app::updater_ui_qa::capture_installed_updater_chrome_offline -- --ignored --exact --nocapture
    if ($LASTEXITCODE -ne 0) { throw "Updater UI fixture failed at $width px" }
    Get-Content -LiteralPath (Join-Path $env:SATIN_QA_UPDATER_OUTPUT 'passed.txt')
}
```

Each state waits for current hits, the expected Winit and canvas geometry, the
local document to settle, and two consecutive identical presented buffers.
The driver pins reduced chrome motion and uses a bounded deadline; a missing
or partial sequence is not evidence. The PNG is the buffer presented to the
native surface, not a screenshot of the desktop compositor.

| File | Required evidence |
| --- | --- |
| `00-no-offer.png` | Settled toolbar before discovery, no badge or popup, full address width. |
| `01-badge.png` | Production update glyph and accent dot, closed flyout, and a shorter address field. Quiet and ellipsis are still in their own hits; the desktop Downloads hit has not moved, and the narrow toolbar does not show it. |
| `02-download-offer.png` | Flyout at its badge-relative/clamped position with a left Download hit. |
| `03-progress-37.png` | Injected 3 of 8 MiB (37% floored); filled and unfilled track pixels and no primary hit. |
| `04-ready-restart.png` | Staged view with only the distinct right Restart hit. |
| `05-dismissed.png` | No flyout surface or primary hit after dismissal. |

Inspect both sets for text clipping and overlap; the fixture checks exact
rectangles and representative badge/progress pixels, not OCR or golden-image
parity. `passed.txt` is written after the six captures and subsequent routing
assertions, including:

- Discovery invalidates old address geometry. While the popup is open but
  not yet presented, an outside click cannot dismiss it or hit the page.
- Escape dismisses the offer; Alt+U reopens it. A click outside dismisses
  without clicking through, including a second click before the closed
  frame. The very same point navigates to `about:version` after dismissal.
- A duplicate Download click requests only one in-memory transfer. Before
  Ready is presented, fresh Enter cannot activate Restart. After Ready is
  presented, the old Download point, a held Space repeat, and synthetic
  Enter do not activate it. A fresh click on the distinct Restart hit reaches
  the test sink once; a duplicate click before repaint does not.
- Held Space is still consumed after dismissal. After its physical release,
  a fresh Space scrolls the underlying offline page.

Clicks use the real mouse handlers; keys use the same physical ownership
guard and App key handler as the Winit dispatch. There is no desktop input
injection. The test opens a new scratch profile and a generated local HTML
file. It passes `enable_updates = false`, so installed discovery and startup
ACK are disabled. Offer/discovery result, progress, staging, and the terminal
Restart recorder live under `cfg(test)` and never create an updater worker
or a `Restart` object. No real check, transfer, staging, activation, child
launch, install/profile access outside the scratch directory, or release
operation is requested. This does not replace package, transport, or
startup-ACK QA.
