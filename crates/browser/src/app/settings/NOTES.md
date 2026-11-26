# Quiet Cinder settings

The HTML reference shows an internal settings tab. Satin keeps its existing
native modal and paints the reference-style page below the dimmed chrome.
It does not create a session tab, privileged URL, or web preference API.
The panel's Polish copy follows the reference; the existing version and
shortcut pages retain their own localization. This is not a language selector.

| Reference category | Native mapping |
| --- | --- |
| Startup | Existing profile-backed tab restoration. The switch controls the next launch and does not immediately rewrite the saved session. The schema, default, atomic write order and unreadable-file protection are unchanged. |
| About | `about:version` and the existing shortcut help route, using normal menu-navigation behavior (current tab). |
| Appearance | Fixed Amethyst styling, not a persisted preference. No palette/tab-shape selectors (hidden in the fixed reference), no bookmark-bar switch until a persistent visibility policy actually controls a native bar, and no animation toggle without a shared motion policy. |
| Privacy | No tracker protection switch without a real network policy. Per-tab back/forward stacks are not a global browsing-history store and must not be presented as one or cleared by a mock action. |
| Search | No selector while omnibox routing is fixed and has no persisted provider contract. Future wiring must change actual query targets, not just a label. |

## Shared `app.rs` hooks

- Keep the existing draw and input entry points (`Panel::draw`, `settings_key`,
  `settings_press`, `settings_release`, and `settings_cursor`). The layout uses
  the shared `TOP` chrome boundary and the current logical window dimensions.
  If chrome height becomes dynamic, use the same content boundary for paint
  and hit-testing rather than covering the new chrome row.
- Call `settings_cancel_pointer()` on `CursorLeft` and `Focused(false)` so a
  lost pointer or window focus clears the pressed/hover states. Call
  `settings_cursor()` after size/DPI changes while the settings modal is open
  to refresh hover against the new geometry.
- Preserve the modal priority for keyboard input. In the baseline `app.rs`,
  an active download intercepts Escape before `settings_key`; guard that
  cancellation with `self.settings_panel.is_none()` or move it below modal
  dispatch so Escape only closes settings while it is open. Keep the
  existing repeat guard on Enter/Space.
- The settings copy is Polish; label the shared menu entry `Ustawienia`.
  The palette constants are local until the chrome has shared Amethyst tokens.
- Like the rest of the canvas chrome, this panel has keyboard focus treatment
  but no platform accessibility tree. If settings becomes a real internal
  tab later, navigation/lifecycle and accessibility need shared work.

## Checks

Existing profile tests cover persisted restart behavior, unreadable file
preservation and write failure. Panel tests cover available tab stops,
direction/wrapping, pointer cancellation and visible hit geometry at desktop
minimum and wider sizes. Manual pass: open Settings, use both Tab directions,
toggle with Space, reopen to check state, and see both About shortcuts. Check
hover/keyboard focus after dragging off a button, leaving the window and
resizing at 100% and high DPI. Check Escape without cancelling a download.
