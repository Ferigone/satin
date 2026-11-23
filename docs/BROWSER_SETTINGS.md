# Browser settings

Open **… → Settings** on the native toolbar. Under **On startup**, **Reopen tabs
from your last session** is on by default: Satin opens the saved tab URLs and
selects the previous active tab, as before. This does not restore each tab's
back/forward stack, form state, or scroll position from a previous process. Turn
it off to launch with one start page instead. A URL explicitly supplied at launch
still opens, and both the **Start page** command and new tabs still use
`about:home`.

Press **Alt+F**, then **Down** twice and **Enter** to open Settings. The checkbox
initially has focus. Use **Space** or **Enter** to toggle it; use **Tab** or
**Shift+Tab** to switch between it and **Done**. **Escape** or activating
**Done** closes the panel. The checkbox saves immediately and the change
applies at the next launch, so closing the panel does not undo it. The setting
does not close current tabs or clear cookies, site data, or shortcuts. As with
any launch, the tabs in the new session become the saved session; choosing the
start page does not keep a second copy of the skipped tabs.

The preference lives in `Settings.v1.json` in the locked profile directory,
separate from `session.json` and site storage. A missing file uses the default
without creating one. An unreadable file is kept as-is and the panel explains
why the default is in use; a failed save leaves the current value unchanged.
Sites cannot open the native settings panel or read or write the preference via
the document runtime. The panel is painted by Satin's native chrome; it has a
visible keyboard focus indicator but does not expose Windows UI Automation
controls.

## Inventory at the alpha.18 baseline

| Area | What Satin already does | Was this a browser preference? |
| --- | --- | --- |
| Startup | Reopens saved tab URLs and restores the active tab; a fresh profile opens the start page. | No; it was fixed before this change. |
| Appearance and tabs | Dark chrome and the approved start-page design; new tabs open that page; `Ctrl` + `+` / `-` / `0` controls current-tab zoom. | No theme or persisted default zoom. |
| Search | Brave is configured separately in the toolbar and in the start-page form. Both also accept addresses, but recognize them differently. | No provider picker. |
| Start-page customization | **Add** saves up to five shortcuts in start-page local storage. The sliders icon opens built-in demos. | A page-local customization, not browser Settings. |
| Privacy and profile | Separate profile lock, session file, DPAPI-protected persistent cookies on Windows, and per-origin local storage. `--profile` selects a profile directory. | No privacy or profile-management UI. |
| About and updates | **About Satin** lists features and gaps; installed builds check for verified updates. | Status and background behavior, not editable preferences. |

## Desktop conventions and proposed hierarchy

- [Chrome Help: homepage and startup](https://support.google.com/chrome/answer/95314?hl=en) separates launch behavior from the Home button and offers a new tab, last session, or specified pages under **On startup**.
- [Edge Learning Center: startup settings](https://www.microsoft.com/en-us/edge/learning-center/how-to-manage-edge-startup-settings) places the choices under **Start, home, and new tabs** and likewise distinguishes opening the browser from Home navigation.
- [Firefox Help: previous session](https://support.mozilla.org/en-US/kb/restore-previous-session) describes an automatic restore preference distinct from restoring the previous session on demand. Satin does not offer the latter here.
- [Microsoft Learn: new tab page search box](https://learn.microsoft.com/en-us/deployedge/microsoft-edge-policies/newtabpagesearchbox) documents separate search surfaces in Edge. In Satin, a single “Default search engine” choice should govern both surfaces and be verified on both before shipping.

Start with the toolbar menu and one **On startup** section. If more supported
preferences land, group them by task: **Startup & new tabs**, **Appearance**,
**Search**, then **Privacy & site data**. Keep shortcuts on the start page and
keep operational views (History, Bookmarks, Downloads) in the browser menu,
not under preferences. Keep About and update status separate. Do not add empty
sections, fake toggles, or controls for a custom homepage, browser-wide zoom,
or site data clearing until those controls have a complete implementation.

## Verification

Build the Windows browser, then run the native smoke scenario with a new
disposable profile. The output directory receives a uniquely named run folder
with PNGs, smoke actions, and JSON receipts.

```powershell
cargo test --locked -p rust-browser app::settings::tests
cargo build --locked -p rust-browser
node tools/verify-browser-settings.mjs target/debug/rust-browser.exe target/settings-smoke
```

The runner launches separate processes. It checks the legacy default, keyboard
toggle, focus traversal, both ways of dismissing the panel, mouse toggle,
startup after restart with both values, and an explicit URL at startup. Only
built-in URLs and an explicitly supplied disposable profile are used. Smoke
input runs through Satin's window handlers, not through Windows keyboard or
mouse automation.
