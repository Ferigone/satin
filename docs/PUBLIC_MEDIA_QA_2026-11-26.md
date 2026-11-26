# Windows public media baseline — 2026-11-26

Source `6412310dc26404075920a85abf51329354206b79`, optimized alpha.45 binary
`7e8be4cf3313d53b2f1939559280d2761e70bf3eb186113aaa2b0462544da872`; Chromium
158.0.8127.88. Both used anonymous, fresh profiles at 1280×800. See
[procedure](PUBLIC_MEDIA_QA.md) for the public targets and controls.

| Site | Satin bootstrapping | Positive Chrome control |
| --- | --- | --- |
| Twitch | 118 executed scripts in both runs; the CDN script exhausted instructions. No video, player or chat marker in either final DOM. | From 1.047 to 6.242 s, 40 to 324 decoded frames, 13,115 to 111,435 decoded audio bytes, no ad marker in that window; five Blob workers observed. |
| YouTube | 50 executed scripts in both runs. Inline script 293 exhausted native work in run 1 and instructions in run 2. Run 2 also hit the script compilation memory limit. No bound player in either capture; a media node was observed at ready state 0 with no dimensions or decoded frames. | From 1.606 to 6.694 s, 53 to 206 decoded frames and 26,536 to 101,408 decoded audio bytes, no ad marker in that window. |

These are bootstrapping failures, not evidence that CSS has no other defects.
Neither public player reached an actionable state in Satin. The known-content
local progressive and three MSE playback scenarios did pass on this same
binary, including pause, seek, resume, finalized completion and rebuffering.
Audio counts do not establish audible output.

The offline capability fixture showed H.264/AAC advertised on both browsers,
but VP9 and AV1 were not advertised in this Satin environment. Satin also did
not expose `MediaCapabilities.decodingInfo`, `requestVideoFrameCallback`, or
`MediaSourceHandle`. Do not assume these are the primary public site blocker
without a trace from the actual site; do not claim support merely to get past
feature detection.

Local receipts: `target/public-media/satin-ZNzNnY/`,
`satin-VR2HlG/`, `chrome-oJb5X7/`, `capabilities-9V1jhs/` under
`target/public-media/`, and `target/native-media-VitGMJ/`.

## Budget attribution

The opt-in debug binary `724972a`, receipt `target/public-media/satin-TRMDFL/`,
reproduced both startup failures. The Twitch task had charged 582,038 units
of native work when it ran out of instructions. Its last native charge was
property-key work; that is not a CPU profile. YouTube's failed native charge
was the insertion step of `Map`/`Set` in `collections.rs`, after 688,492
units of native work. A separate script exhausted the compilation budget in
`lexer.rs`. The two YouTube failures should be tracked independently.

`d3c9e93` changes the string key index to meter hashing once and bound exact
comparison to the corresponding bucket. The authored `Map`/`Set` workload
switches from a budget error to success at the unchanged default limit.
The four suites (VM library, keyed collections, collection helpers, structured
clone) passed 69 tests. See `target/media-collection-delta-721685de869d4371927e42e2c9645a51/`.
The repeat on both public sites (`target/public-media/satin-S3A7rf/`) still
did not create playable media. Twitch exhausted instructions; YouTube now
exhausted instructions and still reported a separate compilation limit. In
those two traces, the last native charge was a lexical name load. It does not
identify the top cost by itself.

`bca25ed` (`target/public-media/satin-mt3AOD/`) measured cumulative native work.
Twitch's `core` task spent 181,224 of 535,683 native units on string property
references. YouTube's inline script spent 169,864 of 647,809 native units in
the RegExp interpreter dispatch and 148,790 on string property references.
These are fuel charges, not elapsed time. YouTube's separate compiler failure
could not debit 384 estimated bytes with 278 left. It was a local accounting
failure, not a measurement of RAM or of the script's full memory requirement.
Neither player became usable on this run.

On `d34d609`, the authored repeated-property workload switched from
budget failure to success at the same limit
(`target/media-property-delta-77d2ba28ceb44a5cab4c520b92f26b00/`).
147 focused JS tests passed. Native progressive/MSE controls and rebuffering
passed (`target/native-media-byzFsG/`). On the public repeat
(`target/public-media/satin-aTfhbw/`), Twitch's last sampled task charged
141,058 units to the uncached property-key path; it still reached the
instruction limit. YouTube still hit both the native-work and compilation
memory limits. No public playback, controls, or Twitch chat were verified.

`7f4e3a7` stores entirely ASCII source at one byte per code unit instead of
four, with Unicode and lone surrogates preserved on their existing path.
140 focused Rust tests and four diagnostic tests passed. All four local
progressive/MSE scenarios passed in `target/native-media-ZhvR61/`.
The current live capture (`target/public-media/satin-DkHnr7/`) still hits
both JS limits. Its failing YouTube compilation processed 2,536,233 of
10,935,744 source code points (23.2%) before exhausting an initial
88,731,616-byte structural allowance. This source used the wide backing
(43,742,976 bytes), so the ASCII improvement did not address that failure.
These are bounded lexer diagnostics, not a measured allocation high-water mark.

## Compiler memory policy

There is no universal compiler-memory quota in ECMAScript. Its
[ParseScript operation](https://tc39.es/ecma262/multipage/ecmascript-language-scripts-and-modules.html#sec-parse-script)
requires early errors before script execution, not a particular parser
architecture. Mature engines keep resource bounds without sizing a parser
quota to particular sites:

- [V8's preparser](https://v8.dev/blog/preparser) checks cold functions and
  resolves scopes without building their full AST and bytecode. V8 also
  [reclaims unused bytecode](https://v8.dev/blog/v8-release-74#bytecode-flushing)
  and has separate [heap controls](https://chromium.googlesource.com/v8/v8.git/+/HEAD/include/v8-isolate.h).
- [Firefox's SpiderMonkey frontend](https://firefox-source-docs.mozilla.org/js/)
  similarly uses lazy parsing and a non-GC compilation representation
  (Stencil). [WebKit's JavaScriptCore](https://webkit.org/blog/9329/a-new-bytecode-format-for-javascriptcore/)
  made retained bytecode more compact. These approaches reduce different
  parts of memory use; none implies that compilation is unlimited.
- [Chromium's renderer architecture](https://www.chromium.org/developers/design-documents/multi-process-architecture/)
  contains a renderer crash to its affected tabs and frames. Process isolation
  is a separate safeguard, not a replacement for bounded compilation.

Satin already derives the classic-document compiler allowance from the
512 MiB VM allocation cap minus estimated heap use and a source/scratch
reservation (`16 * source bytes + 4096` in the baseline, now `13 * source bytes + 4096`
for wholly ASCII scripts). The 12 MiB classic source cap is
separate. The lexer typically debits 384 bytes per structural token, even
though only three lookahead tokens are held. This is an AST/bytecode proxy,
not allocation tracking. Satin already validates and releases one eligible
function body at a time in large, known-strict classic scripts, but that
path still lowers the full body once to check early errors. It does not
establish that the live YouTube script qualifies or that this mechanism alone
would solve its failure.

The unchanged Windows VM passed 82 `parser_budget` and `semantics` tests
on `bca25ed`, including rejection of a dense input under a deliberately small
heap followed by successful evaluation in the same VM, and early errors in
unused function bodies. These are protection controls, not live-site passes.

Keep hard source/depth, task, and aggregate memory protections. Do not raise
an allowance by how much one live script appeared to miss it or tune it to a
domain. Next, measure source/scratch reservation, existing VM use (including
retained code), high-water mark of temporary compiler allocations, and
retained output separately. Compare large generated inputs of different
shapes (dense flat code, nested bundles, large literals, and unused
functions), independent real bundles, and multiple tabs/workers under a
shared memory envelope. Use these results to replace coarse charges with
bounded accounting where possible and to prioritize less eager parsing or
emission. The acceptance tests must preserve early errors in unused
functions, valid closures on first call, and a responsive browser when a
compile is refused. Execution fuel remains a separate investigation.
