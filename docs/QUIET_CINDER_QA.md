# Quiet Cinder: local screenshot QA

**Status: unmeasured.** The protocol and synthetic image tests are not evidence
of parity. The reference is the unmodified `Satin-Quiet-Cinder.html`, SHA-256
`E642B9E492E9DFC738FEFCE3BFD5A09124FCA6E9515DEEEECC373022D41373BD`.
It is a Polish interactive mockup in **Quiet / Cinder (Amethyst) / Capsule**.
Compare Chromium's *web viewport* running that mockup to Satin's *entire native
client* drawing its own chrome and native start page. Loading the mockup
inside Satin or comparing Satin's `--headless` content is not this test.

## Lock the environment

- Use the same Windows host, OS build and 100% scaling (96 DPI, DPR 1) for both
  captures. Use the same SDR/sRGB display; no HDR, Night light, accessibility
  contrast theme, magnifier, remote-session scaling or screenshot resampling.
  Capture **1280×800** (`desktop`), **640×480** (`narrow`, below the 660 px
  breakpoint), and **390×800** (`narrow-390`, below the 410 px breakpoint).
  These are CSS viewport dimensions and exact client PNG dimensions at DPR 1,
  not outer-window sizes. The 390 case is narrow desktop with a mouse, not
  mobile/touch emulation. No OS frame, taskbar, host chrome or cursor in the image.
- Use 100% browser/page zoom, Polish UI/input (`pl-PL`) and `Europe/Warsaw` for
  any dates; exact strings, icon states, tab count/order, focus, pointer,
  scroll offset and selected item matter. The reference runner sets locale,
  timezone, zoom/DPR preconditions and reduced motion. Use corresponding
  candidate locale/preferences where supported; a candidate that lacks the
  locale or equivalent action is a recorded gap, not a matched run. Do not
  translate screenshots or fit them to a different test case.
- Same installed font versions: the prototype requests **Segoe UI Variable
  Text**, then **Segoe UI**/system sans for chrome, and **Georgia** for the
  start wordmark; check actual resolved face and weight, not just the CSS
  stack. Chromium's receipt records sampled platform fonts. Record Satin's
  configured/resolved faces and the Windows font versions (including keycap
  fallback when relevant); if unknown or different, note it and do not infer
  a layout regression merely from text antialiasing. Do not substitute fonts in
  the pinned reference.
- Use a fresh disposable profile per case. Keep the default bookmark bar on
  and focus mode off before each action. Use a desktop pointer, no DevTools,
  browser extensions, initial onboarding or system overlays. Wait for the final
  frame (at least 500 ms after the last input in the native window) with no
  load animation, toast, OS tooltip or context menu. Park the pointer on a
  blank portion of the client, e.g. `(2, height−2)`, unless the case needs hover.

## Capture the reference

From the repo root, using Node 22.3+ and an existing recent Chromium-family
binary supporting modern headless and CDP. This does not install anything.
Run locally with no external content; the HTML is served byte-for-byte over
loopback and the temporary debug port is also loopback-only. Use a fresh output
directory per experiment; record the Chromium binary path/version/hash.

```powershell
$out = 'target/quiet-cinder-qa'
$chrome = 'C:\path\to\chromium.exe'
Get-FileHash -Algorithm SHA256 $chrome
node tools/quiet-cinder-reference.mjs $chrome ../Satin-Quiet-Cinder.html $out
```

The command prints a `chromium-*` run directory containing
`desktop-idle.png`, `narrow-idle.png`, `narrow-390-idle.png`, etc., and
`reference.json` (pin, browser build, viewport, pointer, hashes, fonts and
reference landmarks). Pass a final argument such as `narrow-390/tab-hover`
to capture one state. Each case starts
in a fresh browser context; it does not inherit previous actions or storage.
Use the receipt as a readiness check, not just a PNG. Don't reuse a partial
directory if the capture script errors. Capture twice on the first real run;
resolve drift before comparing to Satin (the address caret can blink; record
the phase and retain both raw runs, not the more favorable score). Do not edit
the HTML, inject CSS, disable controls or fix time-varying geometry by scaling.

## Capture native Satin

Use a staged, uninstalled candidate and record its exact commit, binary SHA-256
and `--version`. Always use explicit `--profile`, never an installed profile.
For real-window runs the staged executable must be outside the updater's
installed location; use a disposable Windows environment if needed. No
production profile migration, network visits or installer runs in this QA.

Start with Home, no back/forward on the active tab, and two inactive tabs
labelled **Granią Secedy**, **Plan wyjazdu** (in that order). Create four
bookmarks in display order: **Granią Secedy**, **Plan wyjazdu**,
**Biblioteka projektu**, **Repozytorium Satin**. Use real native tabs/bookmarks,
not prototype `localStorage`. A local fixture recipe for builds with the
current profile schema follows. Do not reuse this profile between cases. Use
loopback HTTP: `9b20e48` accepts only HTTP(S) entries in the bookmark store.

```powershell
# Set $satin to the absolute path of the staged candidate.
$null = New-Item -ItemType Directory -Path $out -Force
$run = Join-Path (Resolve-Path $out) ('native-' + [Guid]::NewGuid().ToString('N'))
$profile = Join-Path $run 'profile'
$www = Join-Path $run 'www'
$null = New-Item -ItemType Directory -Path $profile, $www -Force
$utf8 = [Text.UTF8Encoding]::new($false)
$port = 8765 # Choose an unused local port and use the same port below.
$titles = @('Granią Secedy', 'Plan wyjazdu', 'Biblioteka projektu', 'Repozytorium Satin')
$entries = @(for ($i = 0; $i -lt $titles.Count; $i++) {
  $file = Join-Path $www "fixture-$i.html"
  [IO.File]::WriteAllText($file, "<!doctype html><meta charset=utf-8><title>$($titles[$i])</title><h1>$($titles[$i])</h1>", $utf8)
  @{ title = $titles[$i]; url = "http://127.0.0.1:${port}/fixture-$i.html" }
})
$session = @{ schema = 1; tabs = @('about:home', $entries[0].url, $entries[1].url); active = 0 }
$bookmarkStore = @{ schema = 1; entries = $entries }
[IO.File]::WriteAllText((Join-Path $profile 'session.json'), ($session | ConvertTo-Json -Compress), $utf8)
[IO.File]::WriteAllText((Join-Path $profile 'bookmarks.json'), ($bookmarkStore | ConvertTo-Json -Depth 4 -Compress), $utf8)
# Run in another terminal with the same $port and $www values; stop after QA:
# python -m http.server $port --bind 127.0.0.1 --directory $www
# Once the fixture server is listening, start the visible candidate:
$satinProcess = Start-Process -PassThru -FilePath $satin -ArgumentList ('--profile "{0}"' -f $profile)
```

After launch, at a width where the seed tabs are visible, activate the two
inactive tabs once to load their titles, then return to Home (Satin may lazily
load restored tabs). Verify tab order and reset the strip to its left edge
before capturing; then resize to the case width. Verify the bookmark order
and use the same initial settled state in every case. Loopback fixtures are
not equivalent to the prototype's `https://*.example` site identities;
record different favicons/labels as differences. For appearance settings,
start with the candidate's stock design configuration; document any necessary
non-default setup and do not auto-enable a feature the suite is meant to test.

For a visible window, keep the window restored, mostly centered and fully
inside one monitor, then resize and capture using Windows PowerShell, for
example:

```powershell
.\tools\quiet-cinder-window.ps1 -SatinPid $satinProcess.Id -Viewport narrow-390 -Resize
# After invoking the capture, return to Satin and do the case before the timer elapses.
.\tools\quiet-cinder-window.ps1 -SatinPid $satinProcess.Id -Viewport narrow-390 `
  -Png (Join-Path $run 'narrow-390-tab-hover.png') -DelaySeconds 8 |
  Set-Content (Join-Path $run 'narrow-390-tab-hover.window.json')
```

Bring Satin to the foreground without changing its control focus, then perform
the action before the delay expires. The tool validates exact client bounds
and DPI and records cursor coordinates relative to the client; still inspect
for occlusion or tooltips. Do not move the mouse while saving. Reject captures
that show another window or a mismatched action state.

Early candidates (including `9b20e48` and `2bfe648`) support a 390 px logical
minimum in a restored window but clamp `--smoke-window --width 390` to 640
physical pixels; use the visible-window method for those binaries. Builds
with the 390 px CLI floor may use `--smoke-window --width 390 --height 800`
at 100% scaling for settled, non-hover states. Require an actual 390×800
PNG or client rectangle; never crop, resample, or count a different size.
A binary that cannot create that client is a viewport gap.
For non-hover cases at `desktop` and `narrow`, `--smoke-window` can instead save its
native client canvas, if its synthetic actions reach the same state. Verify
the JSON receipt (`window_smoke`, `frames_presented > 0`, expected URL/title,
no layout or paint limits) and check the screenshot. For example, with a
disposable `--profile`:

```powershell
& $satin --profile $profile --smoke-window (Join-Path $run 'desktop-idle.png') `
  --width 1280 --height 800
```

The synthetic action file (`--smoke-actions`) accepts `key`, `text`, and
`click_at` in logical coordinates, but `9b20e48` has no move-only
action. Don't replace hover with a click, an Alt+F menu selection or a simulated
web tooltip; use the visible window for those cases. If switching capture paths
changes rasterization, don't mix capture paths within an interaction sequence.

## Action and data matrix

Run the following at all three sizes when applicable; reference case names are also comparator
case names. Use the actual control in each app, not coordinates transplanted
from the design. Check the same starting state before each row.

| State | Action and observation | Comparison boundary |
| --- | --- | --- |
| `idle` | Settle on Home, no popup/focus, cursor parked. Check label, logo, shortcuts, contours and start search. | Caption, caption controls, tabs, left/right toolbar, address, bookmarks bar, wordmark and search crops. |
| `tab-hover` | Hover the label of inactive tab 2, not its close button; wait 500 ms. At 390 hover only the visible sliver of tab 2 without scrolling; its close glyph can be clipped. Active tab unchanged; move off to verify reversal. | Tight inactive-tab crop and full strip. At 390 the clipped face is scored, not hidden text or close controls. No delayed tab preview in this Quiet design. |
| `quiet` | Click the focus-mode control once, park pointer, wait for reflow. Bar disappears and secondary controls hide; tab order and start route unchanged. Toggle back before any additional test. | Toolbar/content seam and repositioned wordmark/search. Missing control = unsupported, not an idle substitution. |
| `menu` | Click the ellipsis, park pointer without moving focus. Record first focused item, anchoring, rows/separators, keycaps, bottom clipping/scroll. | Panel and top rows, including the window-edge gap. Compare only genuinely matching menu content; unsupported rows are separate gaps. |
| `menu-hover` | Open menu by mouse, hover **Nowe okno prywatne** (second row) with no click. | Same panel crops; confirm highlight without accidental activation. Separately review keyboard focus (arrow/tab, Enter on a safe item) in each app; do not use a keyboard-highlight frame as hover. |
| `menu-dismiss` | Open by mouse, park pointer, press Escape once. | Closed crops; verify focus returns to opener or report the separate focus gap. |
| `address` | Click empty address on Home, type `sec` without submitting, park. Keep caret in address and top suggestion selected. | Address and suggestion panel. Ranking/search content is simulated in the prototype; fixture names help, but different native results remain a data or behavior gap. |
| `settings` | From the menu open **Ustawienia**, scrolling the popup as needed; stay in its initial appearance section at scroll top. | Settings header, categories and control rows. Report different setting inventory; test native settings behavior separately. |
| `bookmarks` | From the menu open **Zakładki**, no filter, scroll top. | Header, filter and populated list treatment. Preserve stored native URLs; mock URLs/favicons or a different item inventory prevent a strict list parity claim. |
| `downloads` | From the menu open **Pobrane pliki**, no filter, scroll top. | Design-only image reference until state and text can be matched. See below. |

The prototype seeds its own bookmarks, `.example` pages, clock-relative
history, mock place/search results, and three fake downloads (two completed
and one at `68% · Wstrzymano`). It also simulates profile, private window,
permissions, close/minimize and developer tooling. None of these establish
native equivalence. For real Satin, separately test an empty profile, populated
bookmarks/history via local navigation, and downloads from a loopback server
with a known attachment and completion/cancel gate. Record actual stored
files, network requests and persistence from a second launch if applicable.
Do not edit native storage to invent progress, use wall-clock history as a
pixel gate, or compare a real empty state to fabricated populated data and
call the mismatch a regression. For any unmatched state report
**not comparable** with the reason; the diff may still guide design review.

## Native action smoke: `9b20e48`, 100% scale

These are expected routes from source inspection, not results. Keep native
**routing** observations separate from screenshot scores. At each size use a
fresh copy of the seeded profile and start from Home unless noted. At 390 use
the visible-window receipt. Record actual pointer coordinates, before/after URL
(or active tab/overlay), and whether a click also reached the underlying page.
Repeat the test with the other input method where indicated.

| Probe | Native steps and expected route |
| --- | --- |
| Tabs, overflow and close | Hover the visible part of tab 2 without scrolling, then move out: no activation. At 390 click that sliver and verify only tab 2 activates and is revealed. Open the tab-list chevron, ArrowDown then Enter to select the next tab (wraps at the end); reopen and Escape to leave selection unchanged. Click `+` to add a Home tab; click its revealed `×` to close only it. At 390 check that a clipped close target and the tab/caption gap don't accidentally close, switch or add a tab. Reset for later captures. |
| Start and address | Click the central Home search affordance, then separately use `Ctrl+L`; both should focus native address editing. Enter the fixture-0 loopback URL, wait for **Granią Secedy**, then use Back → Home and Forward → fixture-0. Reload and confirm the fixture is requested again. The toolbar Home button is present at 1280, hidden at 640 and 390; **Strona startowa** in the overflow menu still routes to `about:home`. |
| Overflow and dispatch | Click ellipsis (native center near `(1254,67)`, `(618,67)`, `(375,67)` at the three sizes); separately open with `Alt+F`. Initial selection is **Nowa karta**. Move pointer over **Strona startowa** without clicking: hover moves independently of selection. ArrowDown or Tab then moves selection to that row; Shift+Tab/ArrowUp moves it back. Escape or a click outside dismisses; clicking a divider or panel padding must not trigger an underlying bookmark or page action. See separate gap for the prototype's actual hover row. |
| Menu routes | Open overflow and click **Skróty startowe**; expect `about:start-shortcuts` with title `Start shortcuts`. Repeat keyboard-only: open, Down ×2, Enter. **Skróty klawiaturowe** is distinct: it opens the shortcut help page, not `about:start-shortcuts`. **Historia tej karty** opens per-tab history after the fixture navigation above, not global history. On a local fixture, use menu `+`/`100%` to zoom/reset without navigating, and **Narzędzia deweloperskie** to open the native dock if there is room; close it before parity captures. |
| Bookmarks and settings | From overflow select **Zakładki**; verify the native manager opens and Escape dismisses it. Repeat via the bookmark-bar manager. Click a seeded favorite to navigate to its loopback URL; click the address star twice to remove/restore it and verify the profile store. Select **Ustawienia** from overflow; expect the native settings overlay with startup control, not a new tab. Press Escape and verify the underlying page and tab did not change. |
| Caption | In a disposable visible window press and drag off a caption button before release; it must not fire. Then click minimize/restore and maximize/restore separately, checking that no tab action fires. Leave Close for the end of the case; do not compare an OS-minimized/maximized frame to the restored client PNG. |

For a scripted routing receipt at 1280 or 640, use a fresh profile with no
other process using it. This exercises a mouse hit followed by keyboard dispatch;
it does not reproduce reference menu hover:

```powershell
$width, $height, $menuX = 640, 480, 618 # Or: 1280, 800, 1254
$steps = @(
  @{ action = 'click_at'; x = $menuX; y = 67 }
  @{ action = 'key'; value = 'ArrowDown' }
  @{ action = 'key'; value = 'ArrowDown' }
  @{ action = 'key'; value = 'Enter' }
  @{ action = 'wait_title'; value = 'Start shortcuts'; timeout_ms = 8000 }
)
$actions = Join-Path $run 'start-shortcuts.actions.json'
[IO.File]::WriteAllText($actions, (ConvertTo-Json -InputObject $steps -Compress), $utf8)
& $satin --profile $profile --url about:home --width $width --height $height `
  --smoke-actions $actions --smoke-window (Join-Path $run 'start-shortcuts.native.png')
```

Expect receipt URL `about:start-shortcuts`; this is a Satin-only assertion,
not a scored prototype state. For the three viewport pairs, disqualify
mismatched cases before scoring:

| Case in this harness | `9b20e48` comparison boundary |
| --- | --- |
| `idle`, `tab-hover`, `menu-dismiss` | Compare matching closed state and clipped tab geometry; flag data/font differences separately. Dismissal can be compared visually; a returned keyboard focus indicator must be checked separately, not inferred from a closed panel. |
| `menu`, `menu-hover` | Different native menu inventory and focus treatment. Full menu is diagnostic only; native hover on **Strona startowa** is not hover on prototype **Nowe okno prywatne**. Mark the strict menu pair a gap. |
| `quiet`, `address` | No native Quiet toggle or prototype suggestion panel. The editable address field is supported, but address editing alone does not pass the `address` case. |
| `settings`, `bookmarks`, `downloads` | The native settings overlay and bookmark manager are not the prototype pages. Native real download I/O does not implement the prototype's simulated downloads list at this commit. Mark these strict pairs as gaps, even if a native route works. |

The fixed 390 crops are derived from reference breakpoints: tab-face sliver
at roughly `x=157..205`, toolbar compact controls, bookmark band, centered
Home content and a 352 px popup below the toolbar in the tall client. Verify
against Chromium landmarks before scoring; do not move crops to follow a
different native popup or substitute the 640 geometry.

## Planned fractional DPI and live-resize smoke (not run)

Keep the exact 100% comparator separate. The visible capture helper rejects
anything other than 96 DPI. On already configured 120/144-DPI displays,
record `GetDpiForWindow`, the client physical dimensions, logical dimensions
and pointer coordinates from the same client origin. Planned sizes:

| Logical/CSS client | At 125% (120 DPI) | At 150% (144 DPI) |
| --- | --- | --- |
| 1280×800 | 1600×1000 | 1920×1200 |
| 640×480 | 800×600 | 960×720 |
| 390×800 | 488×1000 (rounded from 487.5×1000) | 585×1200 |

1. Use a display large enough for an unobscured client. Run the routing probes
   at each scale, keeping browser zoom at 100%. Prioritize address vs star vs
   ellipsis, tab face vs close, and menu focus/hover at adjacent row boundaries.
   Check one-pixel seams at logical y=44, 91, 121 and that outline thickness and
   hit targets follow the scale without bleed into neighboring rows.
2. Slowly drag the window across 410, 660 and 880 logical px from both
   directions (check breakpoint −1, exact, +1 at 100%); return to 390. At
   fractional scale, test nearest representable client widths rather than
   silently calling rounded widths exact. Repeat with the menu open and an
   overflowed active tab. Check menu anchor, keyboard selection, edge hit
   targets, clipped/stale hits and active-tab reveal after each redraw.
3. If two monitors at different DPI are available, move the restored window
   across them both ways. Repeat menu and caption hover/press/release,
   including mouse-down before resize and mouse-up after, and verify old
   positions don't trigger actions. Record omissions as not run.
4. For a future pixel comparison at these scales, capture Chromium at the
   same CSS size and DPR and validate exact physical output/rounding on both
   sides before defining scaled crops. At 125%, 390 CSS px is not exactly
   representable as an integer native device width; treat that pair as
   rounding/routing evidence unless a shared exact mapping is demonstrated.
   Do not downsample either frame or feed it to a 100% case.

## Quantify and review

```powershell
# $referenceRun is the printed chromium-* directory.
node tools/quiet-cinder-compare.mjs narrow/tab-hover `
  (Join-Path $referenceRun 'narrow-tab-hover.png') `
  (Join-Path $run 'narrow-tab-hover.png') `
  (Join-Path $run 'narrow-tab-hover.diff.png') |
  Set-Content (Join-Path $run 'narrow-tab-hover.diff.json')
$LASTEXITCODE
```

The comparator accepts exact-size opaque 8-bit RGB/RGBA PNGs (no resize,
translation, masks or color correction). The [fixed crop table](../tools/quiet-cinder-cases.mjs)
uses `[left, top, width, height]` in client pixels. Crops include edges, text and
glyphs, not just sampled flat colors. Each must have at least 0.15% reference
edge pixels (left/above neighbor differs by ≥12 in any RGB channel) so that
an empty/misplaced reference region can't silently pass. The output includes
per-crop counts, changed bounds and maximum channel delta, with whole-frame
metrics **for diagnosis only**. An optional 4×-amplified RGB absolute-diff image
helps locate residuals. Exit codes: `0` all scored crops within budgets, `1`
some crop outside budget, `2` invalid input/reference or invocation, `3`
explicitly recorded gap (no PNGs read or scored). For example:

```powershell
node tools/quiet-cinder-compare.mjs narrow-390/quiet --gap '9b20e48 has no native Quiet toggle'
```

`dataScope` identifies design-only downloads and cases requiring matched data;
an exit code does not verify those preconditions.

Initial, **uncalibrated** budgets per scored crop: mean absolute RGB error
≤3/255 and ≤7% pixels with maximum channel error ≥12/255; tighter 4% on
inactive-tab and menu-top-rows crops. Whole-frame averaging cannot waive a crop.
Before using the first native run to make a pass/fail decision, verify the crop
anchors against the Chromium receipts and repeat the same reference captures.
If a crop misses a feature or a budget needs revision, review/version the
recipe against the reference and documented raster variation before scoring
the candidate. Do not widen thresholds or move regions to hide candidate
differences. Inspect native/reference/diff at 100%, especially glyph shape,
layout shifts, alignment, clipped controls, hover transitions and focus
restoration. Numeric results alone do not validate matching fonts, locale,
interaction or feature coverage.

For each run retain the raw pairs, receipts, diff images/JSON, actual actions
and cursor target, fixture manifest, Windows display/font/locale details,
binary hashes and reasons for any exclusions. Assign each case separately:
matched and within budgets, matched with differences, not comparable,
unsupported, or not run. Keep artifacts in ignored `target/` or local
scratch; this procedure does not publish, install or approve a release.

Lightweight tool tests only (no browser or Cargo):

```powershell
node --test tools/quiet-cinder-compare.test.mjs
```
