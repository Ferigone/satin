# Streaming HTML and parser scripts

`html::HtmlStream` builds a document from decoded UTF-8 chunks. It retains
tokenization state across chunk boundaries and uses the same tree constructor
as `parse` and `parse_fragment_with_context`. The document available at a script
event contains the markup parsed up to that point. The browser can run the
script against that arena and then continue parsing with the same node IDs.

The input is a queue of source pieces, write boundaries and script insertion
points. The tokenizer preserves partial tags, attribute values, comments,
references, CDATA, raw text, script escapes and CRLF normalization across those
pieces. A completed markup lexeme is interpreted once by the shared semantic
tokenizer. Open elements, insertion modes, pending table text, template
contents and active formatting remain in the tree constructor between events.
Formatting reconstruction and the adoption agency retain original start-tag
attributes even when a script changes the corresponding live element. [HTML parsing](https://html.spec.whatwg.org/multipage/parsing.html#parse-state)

## Driving the parser

Create `HtmlStream::new(scripting_enabled)`, call `feed(&str)` with decoded
source, and call `finish()` when the source ends. `next_event()` emits the
following events.

| Event | Driver action |
| --- | --- |
| `Progress` | Inspect new arena IDs and the optional closed element. The range can be empty while a long token is being read. A final progress event can close an EOF-terminated style node. |
| `NeedInput` | Supply more decoded input or close the source. |
| `Script` | Inspect `node`, `kind`, `external` and `depth`. Execute an eligible classic blocking script after `begin_script(node)`. Call `skip_script(node)` when the parser can continue without executing it now. |
| `Paused` | The executing script's insertion point was reached. The driver returns to that script. |
| `WriteComplete` | The tokenizer reached the boundary for the given `WriteId`. Complete tokens from that write have been processed. An incomplete token can still need later input. |
| `Complete` | EOF was processed. |
| `Truncated` | A parser or arena limit stopped parsing. |

`Script` classifies connected, completed HTML script elements as classic
blocking, classic defer, classic async, module, import map or data. Executing
code, choosing whether to execute, fetching resources, processing modules and
dispatching the document lifecycle belong to the browser driver. The parser
reports the start-tag allocation through `Progress` and exposes
`is_parser_inserted_script(node)` for identity tracking. Unclosed scripts at
EOF, scripts inside inert template contents and scripts in a detached open
subtree do not produce execution events. [HTML script preparation](https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element)

Use `take_document()` to move the arena to the host and `restore_document()` to
give it back. While the document is leased, the parser retains its open-element
IDs and refuses to advance. The host may append, detach, reparent and edit
nodes using its normal DOM rules. Preserve allocation order, root ID and the
identity of existing nodes. Removing an open element can leave later parsed
children in its detached subtree until it is popped from the parser stack.
`restore_document()` checks parent/child links, cycles, template contents and
resource limits. The in-crate host can use `restore_document_from_host` with its
existing validated arena and byte count. `document_bytes()` provides the
parser's retained string count, including keys duplicated in attribute
namespace metadata.

## Calling document.write

An active classic blocking script has a relative insertion point immediately
before the next unread input. `write(&str)` inserts its text before that point
and returns a `WriteId`. The host pumps parser events until that write boundary,
running nested inline scripts synchronously. Resume the nested script with
`resume_script(node)` after it returns. Multiple writes preserve call order, and
the tokenizer can complete a tag or reference started by a previous write.

A nested classic external script stays pending while the outer script is
running. `begin_script` returns `NestedExternalPending` in that state. The
driver returns from the write callback, allows the outer script to finish,
and calls `resume_script` for it. The same pending script is then reported at
depth one with the unread written and network input still queued. Once the
external script runs, its writes precede the remaining input. Source EOF is
processed only after these insertion points and their written input are
resolved. [HTML insertion points](https://html.spec.whatwg.org/multipage/parsing.html#insertion-point), [document.write steps](https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#document-write-steps)

## Resource budgets

| Resource | Limit |
| --- | --- |
| Combined source and written input | 8 MiB of UTF-8 per document |
| Retained arena string data | 16 MiB |
| Arena nodes | 100,000 |
| Open elements | 256 |
| Active formatting entries and snapshots | 512 entries and 8 MiB of strings |
| Cumulative parser work | 67,108,864 accounting units |
| Write calls / input pieces | 4,096 / 8,225 |
| Active script insertion points | 32 |
| Input advancement per `next_event()` | Progress after 16 KiB if no token has been emitted |

`feed()` accepts or rejects each chunk atomically. Input advancement yields
between long-token slices. Completing a markup token still interprets that
bounded lexeme in one call. Parser work includes input, tree and formatting
scans, insertion-point operations and full public lease validation. Quota
failures set `truncated` in the parser report and document and return
`ResourceLimit` or `Truncated`. The work counter measures parser operations,
not wall-clock time or JavaScript execution. Use `report()` to inspect counters.

## Verification

The pinned html5lib fixtures and hash list retain their upstream bytes. The
tokenizer suite runs both the contiguous tokenizer and the incremental
tokenizer, feeding one Unicode code point at a time. Each path produces 7,028
passes and eight unsupported cases out of 7,036 initial-state runs. The tree
suite also replays 3,161 supported document runs through the streaming parser
with one Unicode code point per feed and keeps fragment runs on their fragment
entry point. The full passive corpus remains 3,541 passes, four scripted
failures and eight unsupported runs out of 3,553. The passive harness skips
execution at `Script` events. The four scripted fixtures require a runtime
driver check in addition to passive parsing. [Pinned html5lib test revision](https://github.com/html5lib/html5lib-tests/tree/9329e64694e7835d0dcff9811e22856ef6ad16f9)

Focused tests check chunk boundaries, prefix visibility, synchronous nested
writes, external script deferral, lexer state across writes and EOF, stable
arena IDs, formatting mutation recovery, and resource limits on a 1 MiB thread
stack. An owned Chrome run through REA observed the same write ordering,
including the partial reference text and nested external pause. Its evidence ID
is `ev_0518636008e85f031b90acecf7f1313c256dfea38b2df9bb08d1a45bc1c894e4`.

```powershell
cargo test -p rust-browser-engine --test html_conformance --test html_streaming --test html_fragments --test html_foreign --test html_robustness
cargo test -p rust-browser-engine --lib html::
```

The parser accepts decoded strings. Encoding sniffing and incremental byte
decoding are loader responsibilities. This stage covers parser-inserted writes
while a script insertion point is active. The parser API does not implement a
script-created document through `document.open()` and `document.close()`,
post-load destructive writes, speculative preloading, SVG script execution,
custom element reactions, or parser-created legacy form associations.
