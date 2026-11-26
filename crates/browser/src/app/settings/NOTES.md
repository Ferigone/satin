# Quiet Cinder settings

The HTML reference shows an internal settings tab. Satin keeps its existing
native modal and paints the reference-style page below the dimmed chrome.
It does not create a session tab, privileged URL, or web preference API.
The panel's Polish copy follows the reference; the existing version and
shortcut pages retain their own localization. This is not a language selector.

| Reference category | Native mapping |
| --- | --- |
| Startup | Existing profile-backed tab restoration. The switch controls the next launch and does not immediately rewrite the saved session. |
| About | `about:version` and the existing shortcut help route, using normal menu-navigation behavior (current tab). |
| Appearance | The real `Pasek zakładek` switch saves the bar preference. Amethyst and the tab shape remain fixed. No animation toggle until the native motion policy is wired. |
| Toolbar and menu | `Tryb skupienia` is a persisted toolbar toggle and menu command, not a Settings row. It temporarily hides the bookmark bar and the secondary Downloads button without changing the saved bookmark-bar preference or disabling downloads. |
| Privacy | No tracker protection switch without a real network policy. Per-tab back/forward stacks are not a global browsing-history store and must not be presented as one or cleared by a mock action. |
| Search | No selector while omnibox routing is fixed and has no persisted provider contract. Future wiring must change actual query targets, not just a label. |

## Persistence

The existing profile lock and `Settings.v1.json` are used. Startup-only files
load without rewriting: `bookmarks_bar` defaults to `true`, `quiet_mode` to
`false`. Each edit copies the loaded preferences, changes only one value, and
publishes it in memory only after `profile::atomic_write` succeeds. Unreadable
and unknown fields remain protected; an explicitly read-only file retains its
loaded state and disables editing. Other filesystem write failures preserve
the previous value and surface an error. No settings edit rewrites `session.json`.

## Native wiring

1. **One source of toolbar paint, hits, and anchors.** `App::toolbar_layout`
   combines the saved Quiet state and live update badge into one `Toolbar`.
   Draw, hits, Downloads fallback, update popup, and address suggestions use
   that geometry with no second reservation.
2. **Focus command.** Paint the descriptor made by
   `self.settings.chrome().quiet_button(layout.quiet)` and register
   `Action::QuietMode` only from its `hit_rect()`. The descriptor also exposes
   `bounds`, `pressed`, `enabled`, and `label` (`Tryb skupienia`) for status,
   accessibility, or tooltips. In the menu use
   `self.settings.chrome().quiet_menu_label()` and `quiet_enabled()`.
   Both actions call `self.toggle_quiet_mode()`, which
   handles saving, localized error feedback, and geometry invalidation.
3. **Downloads.** Paint, hit-test and show the toolbar activity indicator only
   when `layout.downloads` is `Some`; convert `Box2` with `chrome_box`.
   For the flyout anchor use `chrome_box(layout.downloads_anchor())` in
   both paint and hit geometry. In Quiet this anchors to the overflow button
   or on narrow windows without making it a Downloads hit. Keep `Ctrl+J`,
   the overflow Downloads command, and all transfer behavior
   working; no cancellation on preference changes.
4. **Favorites and viewport origin.** Use
   `self.settings.chrome().bookmarks_bar_visible()` to gate *both* favorites
   painting and their hit registration. `self.settings.chrome().content_top()`
   returns the logical origin (`121` with the bar, otherwise `91`). Feed it into
   `WindowRegions` and use its resulting `page.y` everywhere document paint,
   link/input/scroll hit mapping, anchored select popups, layout/viewports,
   status/chrome backgrounds and dock geometry currently use static `TOP`.
   Paint only one separator when the origins coincide.
5. **Settings modal.** The renderer calls
   `panel.draw_at(&mut canvas, &mut self.fonts, u, &self.settings, regions.page.y)`.
   Settings input already uses `self.window_regions(self.active).page.y`.
   A successful switch save clears old hit maps and runs normal
   window-geometry invalidation.

Keep the existing modal Escape, repeat, pointer cleanup and DPI routing.

## Checks

Profile tests cover legacy startup migration, three independent prefs,
read-only/unreadable preservation and failed atomic writes. Bounds/painting
tests cover both viewport origins, 390-DIP width, both keyboard directions,
disabled switch hits, the bookmark switch's saved state and focus outline at
100%/150%, and Quiet button hover/pressed/read-only paint. After integration,
exercise both toggles across restart, verify a document hit immediately below
the moved toolbar and reopen Downloads with `Ctrl+J` in Quiet.
