# CSS Grid

Grid templates participate in the normal stylesheet cascade, including
custom-property substitution and responsive media rules. Named lines keep
their case and decode CSS escapes. Repeated line-name lists merge at
shared boundaries. Placement accepts signed occurrences, named spans,
and the implicit lines needed when a search runs beyond the explicit grid.
[Grid line placement](https://www.w3.org/TR/css-grid-2/#line-placement)

Grid template areas contribute explicit row and column counts. Each
named cell group must form a rectangle, and each row must contain the
same number of cells. Area names generate start and end line names on
both axes. The `grid-template` shorthand sets the areas and track lists
together. Tracks supplied by area strings take their sizes from
`grid-auto-rows` or `grid-auto-columns` when no template size covers
them.
[Explicit Grid and areas](https://www.w3.org/TR/css-grid-2/#explicit-grids)

Auto-fill and auto-fit expand a fixed-size track pattern before placement.
The repeat count uses the definite preferred or maximum content-box size
when available, or the definite minimum when the axis has no upper bound.
An indefinite axis uses one copy. During intrinsic repeat counting, a
definite minimum supplies the basis for percentage tracks and cyclic
percentage gaps count as zero. Auto-fit collapses unoccupied repeated
tracks and overlaps their adjacent gutters after in-flow placement.
Spanning items keep every track they cross open. Remaining flexible
tracks share the available space using the Grid sizing algorithm.
[Automatic repetition](https://www.w3.org/TR/css-grid-2/#auto-repeat)

Each axis permits up to 128 tracks and a grid accepts up to 4,096 items.
Oversized automatic repetition is clamped and sets the layout-limit flag.
Placement that exceeds the grid or layout work budget uses the existing
flow fallback. Template declarations have a 64 KiB parsing budget,
individual names have a 256-byte limit, and each parsed track list admits
up to 4,096 line-name entries before layout.

The engine supports horizontal standalone grids. Subgrid, the
`grid` shorthand, named placement for absolutely positioned items, and
the resolved Grid track values in CSSOM remain open. Intrinsic sizing
uses the engine's existing content measurement and percentage resolution,
which leaves cyclic percentages and nested intrinsic sizing incomplete.

Run `node tools/verify-named-grid.mjs BROWSER.exe OUTPUT --chrome-reference`
to measure the shared workspace fixture at desktop and compact widths.
The fixture adds an in-flow item and records DOM rectangles before and
after placement. The verifier compares the coordinates with an owned
Chrome process through REA and saves both rendered PNGs.
[Fixture](../tools/fixtures/named-grid.html)
[Verifier](../tools/verify-named-grid.mjs)
