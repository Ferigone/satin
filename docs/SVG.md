# SVG graphics, alpha.5 stage

The engine contains its own Rust SVG scene builder, path flattener, stroke
outliner and software rasterizer. It targets common inline icons and static
illustrations. It does not import an SVG renderer, a layout engine or a
rasterization library. Inline SVG continues to use the browser's HTML foreign
content parser. Standalone SVG images use a separate bounded XML front end.
This is a common SVG subset, not general SVG/XML compatibility.

## Implemented scene features

- Outer and nested SVG viewports, `viewBox`, `preserveAspectRatio` alignment,
  meet/slice/none, viewport clipping, zero-sized viewBox suppression and intrinsic
  replaced sizing with the 300 by 150 default.
- SVG transform lists: matrix, translate, scale, rotate about an optional center,
  skewX and skewY. The caller can supply a final arbitrary affine transform and
  a CSS clip callback without creating an offscreen image for each SVG.
- Rectangles with rounded corners, circles, ellipses, lines, polylines, polygons
  and paths. All absolute and relative `M/L/H/V/C/S/Q/T/A/Z` path commands are
  parsed, including repeated commands, reflected controls and endpoint arcs
  with radius correction. Malformed path suffixes retain the valid prefix and
  set `invalid_geometry`.
- Inherited fill/stroke/currentColor, opacity, fill/stroke opacity, nonzero and
  evenodd winding, visibility and display. Group opacity is isolated before
  compositing overlapping children. CSS geometry winners cover
  x/y/width/height/cx/cy/r/rx/ry, including ellipse auto radii.
- Stroke width, butt/round/square caps, miter/round/bevel joins, miter limits,
  dash arrays/offsets and positive `pathLength` dash calibration. Closed dash
  seams join across the path origin; zero-length dashes retain observable caps.
- Local linear and radial point-focus gradients, local href templates, up to
  256 stops, stop opacity/currentColor, user-space/object-box units,
  gradient transforms, and pad/repeat/reflect spread. Interpolation uses sRGB
  channels and separate alpha, as specified for SVG gradient stops.
- Basic-shape clip paths with child unions, winding rules, transforms and
  user-space units. Object-box clip paths work for individual geometry, including
  percentage lengths in the unit box. Local `use` references support shapes,
  groups, SVG and symbols with viewBox sizing; cycles stop with diagnostics.
- Painted-geometry hit testing in reverse paint order, with clip tests and
  default `visiblePainted` behavior. `use` targets retarget to the instance node.

The CSS subsystem supplies actual cascade winners via `SvgStyle`; presentation
attributes and inline declarations are also understood by the scene builder.
The SVG builder owns inheritance into local resource/use instances. The scene
root needs the containing HTML element's computed color seeded in its override
when that color is inherited from outside the SVG subtree.
When seeding that computed root color, clear its `inherit` marker for `color`.

## Public integration API

`SvgScene::build(document, root_node, viewport_width, viewport_height, styles)`
builds an immutable scene. `styles` is a `BTreeMap<NodeId, SvgStyle>`;
`SvgStyle::parse_property(name, value)` accepts a winner after CSS variable
substitution. `SvgScene::intrinsic_size(document, root)` supplies intrinsic
dimensions for replaced sizing. The caller supplies the actual layout viewport
to `build`, so image scaling and viewBox fitting do not depend on a fixed
intermediate bitmap size.

`paint` maps the scene viewport to a destination rectangle on the caller's
packed `0x00RRGGBB` surface. `paint_transformed` accepts a column-vector affine
matrix `[a,b,c,d,e,f]`, a device clip bounding box and an `inside_clip(x,y)`
predicate for the caller's CSS clip chain. It paints directly into that surface
and returns `SvgRenderReport`; it never reads back or allocates a whole SVG
surface. Rasterization uses four vertical subpixel samples and analytic
horizontal span coverage. Curves are flattened adaptively to 0.1 root viewport
units; this is an approximation and is not a claim of pixel identity with
another browser at arbitrary final zoom.

`geometry_bounds` contains unclipped transformed object geometry without
stroke expansion, including unpainted and visibility-hidden geometry. It unions
rendered group/use descendants. `node_bounds` contains conservative painted
bounds intersected with clips and is intended for broad-phase hit testing.
`hit_test(x,y)` checks actual fill/stroke outlines and clips in local viewport
coordinates. Geometry bounds on curves share the flattening approximation;
these maps do not implement the complete SVG geometry DOM interfaces.

## Standalone SVG XML subset

`svg::parse_document(&str) -> Result<Document, SvgDocumentError>` returns the
existing stable-index DOM arena, with one SVG-namespace root and no inserted
html/head/body elements. It preserves case, namespace scopes, quoted attributes,
XML comments, CDATA, processing-instruction boundaries, XML newline/attribute
normalization, five predefined entities and numeric character references.
Supported element namespace vocabularies are SVG, XHTML and MathML; supported
attribute namespaces are XML, XMLNS and XLink. XLink lookup uses expanded names,
so the declared prefix does not have to be literally `xlink`.
The arena retains element local names and namespace URIs; it does not retain
qualified element prefixes for a complete XML DOM serialization round trip.

`SvgDocumentErrorKind::Invalid` identifies malformed input;
`Unsupported` identifies an unsupported vocabulary/encoding/DTD feature; `Limit`
identifies exhausted resources. Errors include a normalized-source byte offset.
Mismatched end tags, duplicate expanded attribute names, unbound prefixes and
invalid XML characters/references fail instead of receiving HTML tree repair.

The input must already be decoded UTF-8 XML 1.0 with an SVG-namespace root. DTDs,
entity declarations, external entities, other encodings, XML 1.1 and vocabulary
namespaces the current DOM cannot represent are rejected explicitly. In
particular, some editor/RDF metadata is outside this subset. It is not a general
XML processor or validator. Processing instructions do not load stylesheets.
SVG image documents have no script realm; scripts, event-handler attributes and
external resources in an image are never executed or fetched by this pipeline.

## Resource limits and diagnostics

| Resource | Bound |
| --- | --- |
| Standalone source | 4 MiB |
| Standalone retained string accounting | 8 MiB |
| Standalone attributes per element | 256 |
| XML name / namespace URI | 1,024 / 4,096 bytes |
| XML work accounting | 16,777,216 units |
| XML/scene nesting | 64 levels |
| Scene visits, indexed ids, commands | 16,384 each |
| Flattened geometry/stroke vertices | 65,536 cumulatively |
| Scene work accounting | 4,000,000 units |
| Scalar SVG coordinates | absolute value at most 1,000,000 |
| Gradient href chain / stops | 16 / 256 |
| Paint work / painted-pixel operations | 16,000,000 / 4,194,304 |
| Opacity tile | at most 4,096 pixels wide, 64 nested layers |

Scene traversal, curve subdivision and XML parsing use explicit work stacks.
`SvgDiagnostics` reports `truncated`, `invalid_geometry`, `unsupported`, visited
nodes, vertices and work. `SvgRenderReport` reports truncation, invalid caller
surface/transform, work and painted-pixel operations. Invalid surfaces are
rejected before writes. Resource stops can leave a painted prefix; unfinished
isolated opacity layers are discarded. Recognized unsupported effects are
flagged, but these flags are not a universal SVG feature validator: unsupported
CSS declarations can be ignored by the partial cascade.

## Evidence and remaining scope

The seven pinned WPT shape source/reference pairs pass exact RGB comparisons
with no skips. The files, revision, license and byte hashes are recorded in
`crates/engine/src/svg/fixtures/README.md` and `manifest.json`. The denominator is
seven selected pairs, not the complete upstream SVG suite. Rendering both sides
with the same engine checks reftest equivalence; independent pixel/geometric
tests supplement it. Focused Rust tests cover curves/arcs, winding, strokes,
opacity, gradients, clips/use, affine crop/scale, bounds, malformed XML and work
limits, including parse/render/drop on an explicit 1 MiB native stack.

Run the focused checks with the private Rust toolchain configured:

```powershell
cargo test -p rust-browser-engine --test svg_conformance --test svg_document --test svg_rendering
cargo test -p rust-browser-engine --lib svg::
cargo clippy -p rust-browser-engine --test svg_conformance --test svg_document --test svg_rendering -- -D warnings
```

Still missing: SVG text and font shaping; foreignObject HTML rendering; embedded
SVG images; patterns, filters, masks and markers; animation/SMIL; external use or
paint-server resources; full SVG geometry/animated DOM APIs; CSS transform and
transform-origin inside an SVG scene; general vector effects and paint order;
all pointer-events modes; full conditional switch processing; nested clip-path
references, clip-path use/text and object-box clips on groups; linearRGB color
interpolation; SVG2 nonzero focal radii/outside-circle radial cones; percentage
stroke widths/dashes and arbitrary CSS length calculations. SVG `em` and `ex`
geometry currently use fixed 16/8 unit defaults rather than live font metrics.
Extreme transforms/curves and large scenes may hit the explicit budgets. Full
modern website compatibility remains the larger browser goal.

Primary implementation references:
[SVG 2 coordinates](https://www.w3.org/TR/SVG2/coords.html),
[paths](https://www.w3.org/TR/SVG2/paths.html),
[painting](https://www.w3.org/TR/SVG2/painting.html),
[paint servers](https://www.w3.org/TR/SVG2/pservers.html),
[endpoint arc conversion](https://www.w3.org/TR/SVG/implnote.html#ArcImplementationNotes),
[XML 1.0](https://www.w3.org/TR/xml/) and
[XML namespaces](https://www.w3.org/TR/xml-names/).
