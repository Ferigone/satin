# Native chrome review: menu selection

This is an application-only candidate for review after alpha.18 stabilization.
The approved `assets/satin-start-reference.png` (1586 × 992) describes the
closed chrome and start page. The menu is not open in that reference. This
change leaves the closed chrome, start-page assets, toolbar controls, row hit
boxes, and menu actions unchanged.

## Pass 1: installed baseline

Satin 0.1.0-alpha.17 on Windows x64 at 100% scale, executable SHA-256
`9B28B286851B50075979BA5E0604A10FDA7AD5CFC720CB722B29580058C77243`.
The native-window smoke path presents in a hidden window and saves the app
canvas, including chrome. Each run used its own profile, with no desktop
pointer or foreground-focus changes.

Artifacts are under `target/a18-21-smoke-1f34222f24cb41f4946361789f309c22/`
in the chrome-polish worktree:

| Native PNG | Physical size | Synthetic input on `about:home` |
| --- | --- | --- |
| `open-narrow.png` | 640 × 400 | Click menu at logical `(604, 90)`; `Start page` is selected |
| `second-narrow.png` | 640 × 400 | Same click, then `ArrowDown`; `New tab` is selected |
| `second-wide.png` | 1586 × 992 | Click menu at logical `(1550, 90)`, then `ArrowDown` |

The wide closed-chrome comparison was reused from agent 08:
`../satin-a18-08-css/target/a18-08-native/start-1586x992.png` (`about:blank`).

In `second-narrow.png`, the selected fill at `(600, 185)` is `#363537`;
the menu surface at `(600, 150)` is `#2a292b`. The contrast of the old
selection against the surface is **1.19:1**. Separately, the baseline draw
condition hides keyboard selection when the pointer hovers over another row,
even though Enter still invokes the keyboard-selected action.

## Pass 2: selection treatment

The selected row now uses a two-logical-pixel silver outline (`#a9a5a7`),
with **5.01:1** contrast against its fill and **5.95:1** against the panel.
Pointer hover still uses a fill without an outline. The outline stays on the
row Enter will activate, even when the pointer is above or below it. Highlight
edges are rounded to device pixels so adjacent rows do not paint over each
other at fractional scale.

The pixel test in `crates/browser/src/app/chrome_menu.rs` checks 100%, 125%,
150%, and 200% at approximately 640 logical pixels wide, with adjacent hover
above and below selection. It checks the four straight edges for exactly
2/3/3/4 device pixels, respectively, as well as the hover-only and idle rows.
It passed in an isolated Rust 1.99.1 probe linking the existing engine Canvas
artifact, without a Cargo build. `menu-render-only-{100,125,150,200}.png` in
the artifact directory show the same renderer with the first row hovered and
the second selected. These are renderer previews, not native-window captures.
The patched executable and an actual high-DPI Windows window were not run.

## Next review

- On the patched executable, replay the same native smoke actions. Verify that
  the selected ring follows `ArrowDown` / `ArrowUp`, and that `Escape` restores
  the closed chrome. On a Windows test display already at 125% or 150%, use
  800 × 500 or 960 × 600 physical pixels for a 640 × 400 logical client.
- In a separate UI review, check keyboard-only entry to the menu and whether
  Windows contrast themes should drive the chrome palette. This pass does not
  add shortcuts or claim assistive-technology support.
