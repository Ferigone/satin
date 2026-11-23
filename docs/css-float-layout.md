# Float and clearance layout gate

References: [CSS 2 §9.5](https://www.w3.org/TR/CSS22/visuren.html#floats)
and [§10.3.5–6](https://www.w3.org/TR/CSS22/visudet.html#float-width),
[CSS Display 3 §2.7](https://www.w3.org/TR/css-display-3/#transformations),
[Flexbox §4](https://www.w3.org/TR/css-flexbox-1/#flex-items) and
[Grid §6](https://www.w3.org/TR/css-grid-2/#grid-items).

## Design

- Cascade `float` and `clear` as non-inherited longhands. Blockify a floating
  box after cascade: `inline` and `inline-block` become `block`, `inline-flex`
  becomes `flex`, `inline-grid` becomes `grid`, and `none` stays `none`.
  Absolute/fixed positioning wins over float; a flex/grid item does not float
  or clear. CSSOM reads this same computed style and the actual layout boxes.
- In horizontal flow, keep floats in source order and in the coordinate
  space of their block formatting context. Keep the normal-flow cursor
  separate. Size a float against its containing block, not the leftover
  width next to an earlier float. Auto horizontal margins are zero. For an
  auto-sized non-replaced float, clamp the available width between min- and
  max-content, then apply the usual min/max constraints; replaced elements
  use their intrinsic dimensions. Place the real margin box on its side at
  the earliest eligible height, probing the bottoms of earlier floats when
  there is insufficient space.
- Ordinary block borders still follow normal flow. Line boxes inside the
  same context use the space between overlapping float margin edges; if the
  first indivisible item cannot fit, try below the next float bottom.
  Formatting roots isolate their internal floats and include them in
  auto-height; an adjacent independent formatting root must also avoid
  the external floats. Use the existing overflow classification rather
  than making clipping or scrolling a prerequisite.
- `clear` considers only earlier floats in the same context, on the
  requested side(s). When needed, clearance prevents collapse of the cleared
  block's top margin with the preceding block's bottom margin. `clear` on a
  float raises its placement floor. Do not advance the normal-flow cursor
  just because a float was placed or because an irrelevant side has a float.
- Maintain existing layout work limits when scanning exclusions. Complex
  inline rebreaking when the final line height changes, placement of
  independent formatting roots alongside earlier floats, special/replaced
  inline layout, full margin collapsing, and vertical writing modes need
  separate gates. This is not full CSS float conformance.

## Gates

`tools/fixtures/css-float-flow.html` uses ordinary boxes with fixed pixel
dimensions. The passing fixture writes `data-result="pass"`; otherwise it
prints a semicolon-delimited list of failed case names. The browser runner
reports the fixture and binary digests, invocations, exit status, and results.

Run against a digest-verified candidate or reference, with an isolated
profile, from the lane root:

```text
node tools/fixtures/css-float-flow.mjs satin <exe> <sha256>
node tools/fixtures/css-float-flow.mjs chrome <exe> <sha256>
```

The coordinator's engine gate is
`cargo +1.99.1 test --locked --offline -j 1 -p rust-browser-engine --test css_float --test layout_inline`.

For the post-bootstrap Run controls, reuse JS 07's **unchanged** offline
reduction from `183f808e3044df6b627296773019dd224ebe8c21`. The runner reads
the fixture as a Git object, rather than fetching or updating the page:

```text
node tools/fixtures/css-float-flow.mjs satin <exe> <sha256> run-panel
node tools/fixtures/css-float-flow.mjs chrome <exe> <sha256> run-panel
```

At viewport 984×265, require one real wrapper rect with display `block`,
width 160 px in the loaded state and 50 px in the running state, and the
visible button at `[20,0]` from the bar's right and top in both states.
Keep the hidden controls and the existing 800 px panel/462 px bar as
controls. This tests only geometry, not live runner startup or Test262
results; neither fixture is an upstream WPT pass.
