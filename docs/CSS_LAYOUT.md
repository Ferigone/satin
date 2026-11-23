# CSS, layout, and text through alpha.6

The engine compiles selectors and declarations once per layout build. An index of the rightmost selector compound finds candidates by ID, class, and tag. The cascade expands shorthands into longhands and applies specificity, `!important`, inheritance, CSS-wide keywords, and a subset of custom properties.

Layout supports the box model, minimum and maximum sizes, `border-box`, some percentage sizes, adjacent vertical margins, and inline blocks. The Flexbox subset includes direction, grow and shrink, wrapping, gaps, order, auto margins, and alignment. `ElementBox`, `element_at`, and `rect_for` connect geometry to DOM identity. Deep or expensive layouts fall back to a simplified iterative flow and set `Layout.limited`. Tests with 256 levels of block, inline, and flex boxes passed on threads with a 1 MiB stack.

The text shaping cache limits entry count and memory use. Long words break at shaped text clusters in fallback layout, reusing the shaping work. Alpha.6 uses computed CSS families and faces, shared web font data, and character coverage for fallback. [Fonts](FONTS.md)

## Grid and CSS token boundaries in alpha.3

The Grid algorithm handles `display: grid` and `inline-grid`, explicit and cyclic implicit tracks, fixed and percentage sizes, `auto`, `min-content`, `max-content`, `fr`, `minmax()`, and integer `repeat()`. Placement supports positive and negative line numbers, spans, unnamed `grid-area` values, row and column flow, dense and sparse placement, and order. It supports gaps, minimum and maximum container height, percentage item sizes, alignment, stretching, and auto margins. Implicit tracks before the explicit grid start at the correct point in the repeating auto-track pattern. [Later Grid features](GRID.md)

Placement starts with explicit areas, then items with fixed rows, then the remaining items through the auto-placement cursor. The sizing pass resolves intrinsic sizes in increasing span order. Planned track increases make the results independent of item order within a span group. Flexible tracks freeze at their minimum size before redistributing space. During intrinsic container sizing, cyclic percentage rows and gaps use `auto` and zero. Once the container size is known, the percentages resolve against it without a second container sizing pass. Adjacent text forms a single anonymous grid item across comments and hidden nodes. Merged text and whitespace checks are cached for the duration of the layout build.

A grid allows 128 tracks per axis and 4,096 items. Each placement attempt consumes the shared layout work budget. Exceeding it uses the visible flow fallback and sets `Layout.limited`. Storage stays bounded for large requested line numbers. The test at line 32,000 and the 256-level nested grid test retained content and links. The depth test used a 1 MiB stack. Computed styles share track lists, and `rem` and `vw` resolve before layout.

The tokenizer treats comments as token boundaries. It distinguishes `p/**/.x` from `p /**/ .x` and keeps `di/**/v` and `10/**/px` split. Rules and declarations serialize internal boundaries as comments. `var()` substitution also preserves boundaries, so `var(--n)px` stays a number and an identifier when `--n` contains a bare number. The tokenizer normalizes input NUL to U+FFFD before processing internal separators.

The algorithm implements a subset of Grid and CSS Syntax. [CSS Grid Level 1](https://www.w3.org/TR/css-grid-1/#layout-algorithm) · [CSS Syntax Level 3](https://www.w3.org/TR/css-syntax-3/#consume-comments)

## Reference measurements

The alpha.3 comparison used headless Chrome 157.0.8103.51 with an isolated profile, device scale 1, and a 984 × 905 CSS px viewport. Thirty-eight Grid fixtures produced 145 boxes and 580 coordinates. The maximum difference was 0.010422 CSS px. The test `pinned_chrome_157_grid_reference` records the input and expected results with a 0.05 CSS px tolerance. It covers inline box intrinsic sizes, mixed `minmax()` values, multi-track spans, implicit tracks before the explicit grid, cyclic percentages, and minimum and maximum height. Fixtures use zero font size and line height. Their intrinsic sizes come from explicit box dimensions. A separate reference case checked text grouping across comments and `display: none` and confirmed that anonymous items omit the container's padding. [Grid reference test](../crates/engine/tests/layout_grid.rs)

At this stage, the targeted CSS and layout suite passed 35 tests, including the existing Flexbox cases. The benchmark is opt-in.

A separate run used Chrome 157.0.8103.51 with an isolated profile, device scale 1, a 784 × 505 CSS px viewport, and a local document. Eleven rectangles yielded 44 coordinates. The maximum difference was 0.005219 CSS px. The test `pinned_chrome_157_box_and_flex_reference` records the expected geometry for the tested boxes and Flexbox cases. [Box and Flexbox reference test](../crates/engine/tests/layout_geometry.rs)

The alpha.2 release benchmark used 4,000 class rules and 4,000 paragraphs, seven samples, and a warm font cache. The recorded run took 14.761 ms to parse and compile, with medians of 2.692 ms for style computation and 39.000 ms for `Layout::build`. The selector index visited 4,000 candidates out of 16,000,000 possible pairs. The all-pairs count is an operation estimate. Splitting 20,000 and 200,000 ASCII characters took 3.099 ms and 34.994 ms. A prior run recorded 20.447 ms, 5.083 ms, and 34.066 ms for parsing, style, and layout, and 2.946 ms and 29.553 ms for splitting. The timings measure this layout workload and vary across runs.

```powershell
cargo test --release -p rust-browser-engine --test layout_performance -- --ignored --nocapture
```

## Positioning, overflow, and transforms in alpha.5

Layout tracks document flow and visual geometry separately. `position: relative` moves a box and its contents and preserves the in-flow space. Absolute and fixed boxes are out of flow for block, Flexbox, and Grid sizing. Their positions resolve after their containing block dimensions. The containing block is the nearest positioned ancestor's padding box for absolute positioning, the viewport for fixed positioning, or a transformed ancestor when it establishes the relevant containing block. The engine handles physical insets, the `inset` shorthand, percentages, stretching between opposite insets, minimum and maximum sizes, and auto margins.

`overflow`, `overflow-x`, and `overflow-y` support `visible`, `hidden`, `clip`, `auto`, and `scroll`, including paired-axis value computation. Scroll offsets are clamped to the scrollable range. Scrolling moves the contents within the box and leaves the border and background at the box position. Clips and transforms share balanced `PushState` and `PopState` commands. Hit testing for controls, elements, and links uses inverse transforms and the same clips. DOM rectangles expose unclipped visual bounds. Offset geometry comes from the untransformed, unscrolled box.

The 2D transform list includes `matrix`, `translate` and its axis variants, `scale` and its axis variants, `rotate`, `skew` and its axis variants, and `transform-origin`. Transforms compose in CSS order. A singular matrix yields an empty hit region. Transforms, fixed positioning, and applicable `z-index` values on positioned boxes and flex or grid items create stacking contexts painted as a unit. Hit testing follows the paint order, with covering elements taking precedence over links below them.

The layout snapshot stores shared computed styles in an `Arc`, viewport geometry, and nested scroll offsets. `depends_on_viewport_scroll` selects a layout rebuild when viewport movement affects fixed elements. Other pages can update the scroll position directly. The SVG module receives the winning computed values from the cascade. The outer SVG box uses the image sizing path for replaced content.

The positioning reference used headless Chrome 157.0.8103.51, device scale 1, and a 984 × 905 CSS px viewport. It tested 20 cases with 45 boxes and 180 coordinates. The cases include containing blocks, insets, shrink-to-fit, auto margins, minimum and maximum sizes, transforms, and fixed positioning inside a transformed ancestor. The maximum difference was 0.00001145 CSS px with a 0.05 CSS px test tolerance. Other tests check pixel clips, scrolling, `z-index` order, rotated hit regions, and 256 levels of elements on a 1 MiB stack. [Position reference fixture](../crates/engine/tests/fixtures/css_position_reference.json)

A link URL resolves once per element and is shared across text fragments. Link metadata allows up to 64 KiB per URL and 8 MiB total, with exhaustion setting `Layout.limited`. The visual pass walks the tree and stacking contexts iteratively. Clip and command emission have a separate budget of 1,000,000 work units. Each transform list allows up to 32 operations.

This stage covers a subset of horizontal left-to-right layout. Sticky positioning, logical insets, right-to-left layout and writing modes, full static positions for absolute boxes in Flexbox and Grid, positioned fragmentation, 3D transforms and perspective, masks, CSS `clip-path`, rounded clips, and native scrollbars remain open. [CSS Positioned Layout Level 3](https://www.w3.org/TR/css-position-3/) · [CSS Transforms Level 1](https://www.w3.org/TR/css-transforms-1/) · [CSS Overflow Level 3](https://www.w3.org/TR/css-overflow-3/)

## Logical box properties

For horizontal writing, Satin maps logical dimensions, margins, padding, and insets onto the equivalent physical properties. Physical and logical declarations compete in the same cascade, including source order and `!important`. Inline start and end follow each element's computed `direction`, including inherited direction and HTML `dir`. Inline styles and computed styles expose the corresponding logical properties. Vertical writing modes and bidirectional text layout remain open. [CSS Logical Properties and Values Level 1](https://www.w3.org/TR/css-logical-1/#box)

Inline CSSOM recombines the two declared edges of logical margin, padding, and inset shorthands when their priorities and values allow it. Editing or removing one edge preserves the other ordinary edge and its priority. An edge can be lost when its original shorthand still needs variable substitution and the mutation requires splitting that shorthand. [CSSOM declaration blocks](https://drafts.csswg.org/cssom/#the-cssstyledeclaration-interface)

The channel-toolbar fixture matches Chrome 157.0.8103.79 for geometry and live CSSOM updates. The pinned Web Platform Tests fixture covers specified and computed values with custom properties. [Toolbar fixture](../crates/engine/tests/fixtures/css_logical.html) · [Web Platform Tests source](https://github.com/web-platform-tests/wpt/blob/9e1907a13cbbc338975302097a4bafcbc4d12dc5/css/css-logical/logicalprops-with-variables.html) · [License](../crates/engine/tests/fixtures/wpt-url/LICENSE.md)

## Flex item minimums

Computed scrollable overflow on a flex item's main axis gives it a zero automatic content-box minimum. This lets the viewport of a scrolling pane shrink to its assigned space while preserving border, padding, and explicit minimum sizes. A main axis with visible or clipped overflow keeps a content-based minimum. The preview-caption and chat-pane fixture matches 44 rectangle coordinates in Chrome 157.0.8103.79. [CSS Flexible Box Layout Level 1](https://drafts.csswg.org/css-flexbox-1/#min-size-auto) · [Reference fixture](../crates/engine/tests/fixtures/css_flex_overflow_minimum.html)

## Viewport-sized Flexbox shells

Percentage heights on the root element resolve against the initial containing block's viewport height. A chain of definite percentage heights can then fill the viewport without making percentage heights under auto-sized block parents definite. The reduced fixture is based on Twitch's static loading shell and records 48 CSSOM rectangle coordinates at 1280 × 800 and 640 × 480 CSS px in Chrome 157.0.8103.79. In alpha.17, the same reduction collapsed to 72 px in both viewports. This checks the shell, not Twitch's JavaScript app or its player. [CSS 2.2: containing blocks](https://www.w3.org/TR/CSS22/visudet.html#containing-block-details) · [Reference fixture](../crates/engine/tests/fixtures/css_flex_viewport_shell.html)

## Other limits at alpha.6

At alpha.6, remaining Grid features included named lines and areas, `grid-template-areas`, `auto-fit`, `auto-fill`, `fit-content()`, subgrid and masonry, baseline alignment, vertical writing modes, fragmentation, and replaced-element sizing with aspect ratios and constraints on both axes. The Grid feature notes describe subsequent additions. CSS Syntax, Selectors, CSSOM, and Flexbox have partial coverage. Parent-child margin collapsing, full bidirectional and Unicode text support, broader font matching, and the CSS Font Loading API remain open at this stage. [Grid](GRID.md) · [CSSOM View](CSSOM_VIEW.md) · [Fonts](FONTS.md)
