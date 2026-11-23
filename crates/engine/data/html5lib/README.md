# HTML parser evidence and remaining scope

This directory contains test data, not a third-party parser implementation.
The browser's tokenizer and tree constructor are independently written in Rust.

Source: [html5lib/html5lib-tests](https://github.com/html5lib/html5lib-tests),
pinned commit [9329e64694e7835d0dcff9811e22856ef6ad16f9](https://github.com/html5lib/html5lib-tests/tree/9329e64694e7835d0dcff9811e22856ef6ad16f9).
This is the last upstream tree-construction revision before those tests moved to
[Web Platform Tests](https://github.com/web-platform-tests/wpt/tree/master/html/syntax/parsing).
All 57 top-level tree-construction `.dat` files, all 3 `scripted/*.dat` files,
and all 14 tokenizer `.test` JSON files were copied byte-for-byte.
`SHA256SUMS` records their hashes. `LICENSE` and the two `FORMAT.md` files retain
the upstream license and test-format instructions. Upstream fixtures are never
rewritten to match this parser's results.

Algorithms are based on the primary
[WHATWG HTML parsing specification](https://html.spec.whatwg.org/multipage/parsing.html)
(consulted 2026-11-10). The pinned corpus is not every current WHATWG or WPT test.

## Measured results

Measured on Windows x64, Rust 1.99.1, 2026-11-10:

| Measurement | Pass | Fail | Unsupported | Total |
| --- | ---: | ---: | ---: | ---: |
| Tree output, required scripting-flag variants | 3,541 | 4 | 8 | 3,553 |
| Token output, required initial-state variants | 7,028 | 0 | 8 | 7,036 |

The tree suite contains 1,796 source fixtures. Cases without a scripting flag
are run twice, with scripting disabled and enabled, as the upstream format
requires. Explicit `#script-on` and `#script-off` cases run in that mode.
The four measured tree failures are listed in `known-failures.txt`: they require
synchronous script execution during parsing, script-driven mutations of open
elements, or `document.write`. Setting the HTML scripting flag does not execute
scripts. Post-parse JavaScript execution in the browser does not fill that gap.

All 562 formerly excluded SVG/MathML tree case/mode runs now pass: 126 foreign
fragment-context runs and 436 namespace/integration-point runs. All 56 formerly
excluded CDATA tokenizer initial-state runs also pass. No fixture, denominator,
or known-failure entry changed to achieve these results.

The remaining unsupported tree case/mode runs are:

| Category | Runs |
| --- | ---: |
| `selectedcontent` form-control DOM algorithms | 8 |

Unsupported tokenizer runs are 4 isolated UTF-16 surrogate inputs that cannot
be represented by Rust `&str`, and 4 XML
infoset-coercion cases. These are not counted as passes. The production decoding
layer supplies valid Unicode to this parser. Parse-error counts, codes and source
locations are not implemented and are not compared in either suite.

The tests compare complete DOM trees or complete token sequences, retaining
comments, doctypes, attributes and text. Token-character chunks are coalesced
only as the upstream tokenizer format specifies. DOM parent/child consistency
and absence of cycles are also checked. Expected failures must match the exact
baseline; new mismatches fail the Rust test, and resolved mismatches require
removing their baseline entry. Fixture and mode counts are asserted to prevent
accidental loss of coverage.

## Reproducing the checks

From the repository root, with the project's Rust environment configured:

```powershell
cargo test -p rust-browser-engine --lib html::tokenizer_tests -- --nocapture
cargo test -p rust-browser-engine --test html_conformance -- --nocapture
cargo test -p rust-browser-engine --test html_robustness
cargo test -p rust-browser-engine --test html_fragments
cargo test -p rust-browser-engine --test html_foreign
```

Set `HTML_CONFORMANCE_VERBOSE=1` to print each mismatch and unsupported case.
The ignored `complete_html5lib_tree_audit` test can also audit an external
checkout by setting `HTML5LIB_TREE_DIR` to its `tree-construction` directory.
That opt-in audit prints measurements; it does not claim universal conformance.

## Implementation boundary

Implemented mechanisms include implicit `html`/`head`/`body`, insertion modes,
scope checks and optional end tags, active-formatting reconstruction and the
adoption-agency algorithm, table/caption/column/row/cell modes and foster
parenting, pending table text, comments, doctypes and document quirks modes,
attribute and character-reference tokenization, raw text/RCDATA/script data,
script escape and double-escape transitions, plaintext and newline preprocessing.
Current `select` rules use normal body parsing and scope boundaries rather than
the removed legacy select insertion modes.

HTML fragment parsing now initializes the tokenizer and insertion modes from
the context element, including table/row/column contexts, RCDATA/raw text/script,
select fragment exceptions, scripting flags and template contexts. Its raw-text
contexts have no appropriate end tag, as required by the fragment algorithm;
apparent closing tags for the context remain text. Template parsing implements
the template insertion-mode stack, nested templates, detached inert contents,
form-pointer rules, table insertion redirection and EOF recovery. These changes
enable all previously excluded HTML fragment/template corpus cases without
altering any upstream fixture, denominator or known-failure entry.

`parse(&str)` defaults to scripting disabled. `parse_with_scripting(&str, bool)`
sets the HTML scripting flag, including correct `noscript` treatment. DOM nodes
keep stable arena indices when recovery reparents them. Historical detached
formatting nodes may remain in the arena; `elements` enumerates connected nodes
in document order. Text collection and serialization use explicit work stacks.

`parse_fragment(source, context_tag, scripting)` returns a Document whose root
children are the explicit fragment roots. Synthetic parser nodes remain detached
in the arena, preserving indices; no document-wrapper element is connected to
ordinary fragment results. The `html` context still produces `head`/`body` as
the standard's context-specific insertion modes require. For a real DOM owner,
`parse_fragment_with_context` also accepts inherited quirks mode and a nearest
form-ancestor flag through `FragmentContext`.

SVG and MathML parsing implements the adjusted-current-node dispatch rules,
MathML text and HTML integration points, foreign-content breakout and end-tag
recovery, self-closing elements, SVG local-name and attribute adjustments,
MathML `definitionURL`, and the standard XLink/XML/XMLNS attribute namespaces.
CDATA is recognized only with a foreign adjusted current node and emits literal
text. Foreign fragments initialize from their namespace and context attributes;
MathML `annotation-xml` checks its `encoding` value for HTML integration. HTML
raw-text fragment states apply only to HTML contexts. Namespace-aware scope
boundaries, foster-parenting checks and template rules preserve mixed trees.

`Document.namespaces` holds `Namespace::Svg`/`MathMl` overrides by element ID;
absent entries default to `Namespace::Html`, including elements added through
the existing arena append API. `namespace`, `namespace_uri`, and `local_name`
expose identity without changing `NodeKind` or `NodeId`. Namespaced attributes
retain qualified keys in the element's existing attrs map, with metadata in
`Document.attribute_namespaces`, keyed by `(NodeId, qualified_name)`.
`attribute_namespace`, `attribute_local_name`, and `attribute_prefix` expose
their identity. Fragment imports and cloning must remap/copy both metadata maps
with each element, including descendants inside template contents. Ordinary
attributes containing a colon have no namespace unless that metadata exists.

`elements(tag)` retains its namespace-blind local-name search. `html_elements`
filters HTML namespace elements, and `title()` uses only HTML titles. Resource
loaders and HTML DOM algorithms must use namespace filtering rather than treating
an SVG `script`, `style`, or `title` as its HTML counterpart. HTML serialization
retains adjusted foreign names, qualified namespaced attributes and closing tags
for foreign elements even when their local name is an HTML void-element name.
Foreign `script`/`style` text is escaped; HTML raw text follows HTML rules.
These parser capabilities do not supply SVG/MathML rendering or SVG script
execution. Parser-time HTML/SVG script execution remains outside this stage.

`Document.template_contents` maps each template element to a distinct detached
Document-kind container. The template's ordinary child list is empty; contents
have normal parent/child links rooted at the detached container. `text`,
`connected_nodes` and `elements` omit these inert contents. `serialize_node`,
`inner_html` and `serialize` include template contents explicitly. Embedders
importing a fragment must remap both template keys and container values and must
preserve these ownership relationships; detached containers use the same arena.

Known work beyond the measured subset includes the unsupported categories above,
parser-blocking scripts and reentrant input, current processing-instruction
syntax added after this pinned corpus, parse-error diagnostics, byte-encoding
sniffing/restarts, declarative shadow roots, sanitizer-specific/streaming parsing
and the complete current WPT suite.
Recording quirks mode does not by itself implement all quirks-mode layout rules.

Resource budgets are 8 MiB of UTF-8 input, 100,000 arena nodes (including detached
recovery nodes), and 256 simultaneously open elements. Reaching a budget returns
the partial DOM and sets `Document.truncated`. Input truncation respects UTF-8
boundaries. Tests cover each budget, 100,000 ignored end tags, UTF-8/raw-text edge
cases, both scripting flags and 2,048 deterministic malformed token sequences.
The fragment suite adds 1,088 deterministic malformed context cases, all fragment
budgets, template ownership invariants, serialization, form/quirks inheritance
and raw-text context edge cases.
The foreign-content suite adds namespace and attribute metadata checks,
integration-point and foreign-fragment regressions, literal CDATA, foreign
serialization rules, template ownership across namespaces, 640 deterministic
malformed foreign-context cases, and node/depth budgets on an explicit 1 MiB
native thread stack. At a resource halt, the last attached foreign element
retains its namespace and namespaced attributes.
These results represent a substantial alpha-stage subset, not full HTML5 or
modern-web compatibility.
