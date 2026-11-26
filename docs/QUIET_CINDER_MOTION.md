# Quiet Cinder native chrome motion

Reference: `Satin-Quiet-Cinder.html`, final `.icon`/`.tab-x` overrides and
base `.window-buttons`/`.tab` rules. This slice animates paint in the running
Rust browser; it does not change tab layout or the behavior of the controls.

| Live paint target | Duration | Curve |
| --- | --- | --- |
| Toolbar and tab-strip button hover background and icon ink | 130 ms | CSS `ease` |
| Windows caption hover background and Close ink; existing Close-pressed tint | 160 ms | CSS `ease` |
| Inactive tab hover fill | 170 ms | `cubic-bezier(.22,.85,.25,1)` |
| Tab label color and selected outline | 170 ms | CSS `ease` |
| Tab Close glyph opacity and hover background | 120 ms | CSS `ease` |

`ease` is `cubic-bezier(.25,.1,.25,1)`. Enter/leave reversals continue from the
current sampled value with CSS binary transition shortening. The selected tab
gradient switches discretely (`background-image`); its outline/ink animate.
Caption press/release behavior is unchanged. Toolbar press scaling, address
focus effects, loading, route transitions, and panel entry are out of scope.

## Integration contract

- `crates/browser/src/app/chrome_motion.rs` is a dependency-free timing model.
  `App::draw` samples at one monotonic timestamp before painting chrome and
  only arms a wake after successful presentation. Each wake is one-shot, at
  most one nominal 16 ms refresh interval away and no later than the nearest
  active transition endpoint. A missing or failed redraw does not repeat it.
  `App::about_to_wait` merges it with the existing deadlines; a motion-only
  wake requests a frame without the layout/title work in the normal update arm.
- Chrome motion does not edit hit rectangles, cursor routing, tab selection,
  click dispatch, or overlay precedence. Tabs keep motion state by `Tab::id`,
  not by position, and unseen tabs are retired after a frame. Storage is
  bounded by nine button tracks, three caption entries, and at most two
  visible tab sets (48 entries total) during a frame. No hit becomes smaller
  or moves while its pixels change.
- On Windows, the browser snapshots Win32 `SPI_GETCLIENTAREAANIMATION` at
  startup and on window-focus gain. If disabled, targets snap and no chrome
  motion timer is used. There is no OS change listener while the window stays
  focused. Non-Windows uses its prior animation default; on any platform
  `SATIN_REDUCED_MOTION=1` disables this chrome motion at startup. Native smoke
  screenshots also snap to final paint. None of these changes defines a web
  `prefers-reduced-motion` value or changes the existing scroll policy.

## Retest

1. After the shared build slot is free, run
   `cargo test -p rust-browser chrome_motion` for deterministic time, reversal,
   easing, tab identity, reduced-motion, and wake tests.
2. In a live window, move across toolbar controls, Windows captions, and several
   tabs. Enter and leave quickly. Verify old and new hover surfaces can fade
   concurrently, disabled Back/Forward do not light up, and the paint settles
   after the pointer stops. Select a visible inactive tab and check that the
   gradient switches and the old/new outlines settle.
3. Switch or close tabs while a hover is fading, and scroll the strip. Verify
   a different tab does not inherit the closed tab's transition. Right-click
   empty titlebar space and check resize/drag and close still work as before.
4. On Windows, turn **Settings → Accessibility → Visual effects → Animation
   effects** off, return focus to Satin, and repeat; all targets in the table
   should snap. On another platform or for QA, launch a fresh process with
   `SATIN_REDUCED_MOTION=1` and expect the same result.
