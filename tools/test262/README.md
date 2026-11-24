# Test262 runner

The runner sends test source to `rb-js`, the Satin JavaScript engine's test worker. Node handles YAML metadata, file selection, and process supervision. The worker runs each variant in a fresh VM.

For the public legacy Test262 site instead, see the [separate live-browser probe](website.md). The numbers are not interchangeable.

Use a [Test262 checkout](https://github.com/tc39/test262) at commit `48221d7c1f7745dc13e420a2e850a248c0b2afb4`. The runner reads source and harness bytes from Git objects and verifies blob hashes. Working-tree line endings leave the parser input unchanged.

From the repository root, run

```powershell
cargo build -p rust-browser-js-harness --release --locked
npm --prefix tools/test262 ci --ignore-scripts --no-audit --no-fund
node tools/test262/runner.mjs --test262 PATH_TO_TEST262 --worker target/release/rb-js.exe --path language/expressions/addition --path language/expressions/multiplication --path language/expressions/subtraction --path language/expressions/strict-equals --path language/statements/if --output target/test262/report.json
```

The runner prepares sloppy and strict variants according to the `noStrict`, `onlyStrict`, `raw`, and `module` flags. A `module, raw` test still uses the module grammar but loads no harness scripts. Non-raw variants load upstream `assert.js`, then `sta.js`, followed by the requested `includes`. Non-raw `async` variants also load upstream `doneprintHandle.js` before `includes`. Parse and early errors are checked before execution. The harness must succeed before a negative runtime test can count its expected error.

`$262.evalScript` executes a classic script in the current realm's global environment, using the caller's remaining task budget. It returns the script completion or propagates the script's exception. The UTF-8 parser does not yet accept source text containing isolated UTF-16 surrogates.

`$262.createRealm()` creates a separate global and intrinsics in the same Satin heap. Its host supports nested realms, script evaluation and printing. The main agent exposes a monotonic clock. Operations that require another agent, and access to `$262.gc` or `$262.AbstractModuleSource`, record the missing facility as `unsupported` when encountered during setup or evaluation. The worker aborts the variant rather than returning a dummy value or a catchable JS error. Other upstream setup failures remain `harness_error`.

## Modules

Module variants use the separate strict module grammar. The parser supplies import requests in source order with their exact UTF-16 units. The runner reads relative source files from pinned Git blobs and sends a dependency graph to the worker. The VM links and evaluates live bindings, namespace objects, cycles, and `import.meta`. Harness files run as separate scripts in the same VM.

Negative resolution tests require the expected error class during linking. Host setup, missing fixtures, and entry syntax errors get separate outcomes. Negative runtime tests execute the linked graph. Top-level await suspends module execution until the relevant Promise jobs run. A graph still suspended after the queue drains receives `async_incomplete`.

Dynamic import, top-level await, and ShadowRealm cases receive their original relative fixture blobs. The worker resolves each request in its originating realm and caches module records by realm, file, and module type. A dynamic fixture set is limited to 1,023 files and 4 MiB of source. A static graph is limited to 1,023 fixtures, 16,384 edges, and 8 MiB of source. The parser accepts dynamic import options and source/defer call syntax. Evaluation-phase imports support JavaScript and synthetic JSON, text, and immutable byte modules. A JavaScript module rejects a source-phase import with SyntaxError. Deferred evaluation, hosts backed by separate module-source records, external URLs and agent hosts remain open.

## Asynchronous tests

Upstream `doneprintHandle.js` supplies `$DONE` to asynchronous tests. The worker drains a shared FIFO queue for Promise reactions, thenable assimilation, queued microtasks, and async function resumes. Source and checkpoint share one execution budget.

The worker observes the first argument to `print` as required by Test262. Passing requires one `Test262:AsyncTestComplete` and zero `Test262:AsyncTestFailure:` markers. Extra completions or failure markers fail the test, even after the diagnostic log fills. With an empty queue and no pending finite atomic timeout, absence of the completion marker yields `async_incomplete`.

After the checkpoint, the worker checks unhandled rejections against the rooted original `Test262Error` prototype chain. In an async test, an unhandled rejection with that prototype fails the test, including one that follows `$DONE()`. Synchronous variants record all unhandled rejections as diagnostics. Async variants record other rejection reasons as diagnostics. A handler added before the checkpoint removes the source Promise from the unhandled list.

Timers and external worker or agent hosts are unsupported. Unsupported language constructs receive `unsupported`. An upstream harness execution error receives `harness_error`.

The worker provides a callable `$262.IsHTMLDDA` object with the Annex B truthiness, equality and `typeof` semantics. The worker runs as a non-blocking agent. It executes `CanBlockIsFalse` variants and records `CanBlockIsTrue` as unsupported. Finite `Atomics.waitAsync` deadlines can resume a pending asynchronous test or module within the configured wall-clock limit. All variants remain in the selected denominator. The runner does not skip variants just because they declare arbitrary language features.

## Reports and limits

The report records the pinned revision, worker and runner SHA-256, selection, results per variant, and configured timeout and instruction limits. Pass rates apply to the selected paths. Exit code 1 indicates at least one non-passing result. Exit code 2 indicates a configuration or runner failure.

Repeat `--path` to select paths relative to the Test262 `test` directory. Use `--limit` and `--offset` to select a subset. The default `--timeout-ms` is 3,000 and the default `--instructions` is 1,000,000. `_FIXTURE` files are excluded from directory selections and cannot be requested as standalone tests.

At the pinned revision, 441 generated Unicode property-escape files construct strings covering every code point from U+0000 through U+10FFFF. In both modes, the inputs together span 1,114,112 code points and 2,162,688 UTF-16 code units per file. The upstream range loops execute at least 1,112,858 iterations per file. With this interpreter's scalar loop execution and the one-million-instruction profile, the 882 variants exceed the budget during input construction or subsequent checks. Each remains in the denominator. [Upstream helper](https://github.com/tc39/test262/blob/48221d7c1f7745dc13e420a2e850a248c0b2afb4/harness/regExpUtils.js) · [ASCII property test](https://github.com/tc39/test262/blob/48221d7c1f7745dc13e420a2e850a248c0b2afb4/test/built-ins/RegExp/property-escapes/generated/ASCII.js)

To measure the entire pinned suite, use `--path .` with the default offset and no limit. With the release worker and checkout prepared as above, run

```powershell
node tools/test262/runner.mjs --test262 PATH_TO_TEST262 --worker target/release/rb-js.exe --path . --output target/test262/full.json
node tools/test262/summarize.mjs target/test262/full.json
```

On Linux, use `target/release/rb-js` for the worker. The 2026-11-24 snapshot used Linux x86_64 with seven workers, each running disjoint ranges of up to 1,000 files at the default budgets. VM stack limits vary by platform.

The summary counts files and variant outcomes overall and by top-level test folder. A file passes when every reported variant passes. An invalid-metadata row counts as a non-passing file and row. For sharded runs, pass all report filenames to the same command. It reads JSON or gzip-compressed JSON, checks that the revision, worker, runner and budgets match, and rejects duplicate variants. Totals cover only the supplied reports.

The runner sends at most one request at a time. JSONL requests are capped at 16 MiB. The worker bounds the execution budget to 1–100,000,000 instructions, charges native work and string copies, and limits each checkpoint to 10,000 jobs. The VM queue defaults to 4,096 jobs. Reaction and unhandled-rejection limits also apply. The log holds up to 64 entries, each with up to 2,048 UTF-16 units of preview.

The VM defaults to 64 MiB of estimated allocations and 100,000 heap cells, with additional source, text, and nesting limits. The allocation budget estimates VM memory. Reaching a resource limit yields `resource_limit`. The outer deadline stops only the worker process created by the runner and records `timeout` or `crash` as appropriate. [JavaScript engine documentation](../../docs/JAVASCRIPT.md)

### Refresh the README compatibility snapshot

After a full run, update the date, Satin commit and build environment in [the snapshot](../../docs/compatibility/snapshot.json). Review its CSS and web measurements against their linked fixtures. From the repository root in PowerShell 7, run the commands below. For shards, pass all shard paths in place of the single input file.

```powershell
$summary = node tools/test262/summarize.mjs --outcomes target/test262/outcomes.tsv.gz target/test262/full.json
if ($LASTEXITCODE -ne 0) { throw 'Invalid Test262 reports' }
$json = $summary -join "`n"
if (!(($json | ConvertFrom-Json).fullSuite)) { throw 'The reports cover only part of Test262' }
Set-Content docs/compatibility/test262.json -Value ($json + "`n") -Encoding utf8NoBOM -NoNewline
Copy-Item target/test262/outcomes.tsv.gz docs/compatibility/test262-outcomes.tsv.gz -Force
node tools/render-compatibility.mjs
```

`fullSuite` requires every input to select `--path .`, agree on the available file count, and together cover that many distinct files. The gzip contains one headerless TSV row per variant, ordered by path then variant. Its columns are path, variant (empty for invalid metadata), and status. The summary records the SHA-256 of the uncompressed UTF-8 bytes with LF line endings. Review the snapshot date, Satin source revision and build environment when publishing new results.

## Compare two reports

From the repository root, pass the baseline report first:

```powershell
node tools/test262/delta.mjs target/test262/baseline.json target/test262/candidate.json
```

The comparator needs only Node and the two schema-1 JSON reports. It rejects different Test262 revisions, `--path` filters, offsets, or selection limits; path filter order and duplicate filters do not matter. Other spellings of a path must be kept identical between runs. The JSON delta records the revision, filters, and worker and runner hashes (if present). It also rejects unequal `--timeout-ms` or `--instructions` values when both reports record them. Comparisons involving legacy reports without budgets are marked `limits.verified: false`.

The delta compares statuses for each `(path, variant)` pair, including a `null` variant for invalid metadata. It ignores test order, timings, and diagnostic text.

- A regression is a matched variant that went from `pass` to a non-passing result, such as `fail`, `unsupported`, or `timeout`. The reverse is an improvement. A change between two non-passing outcomes is reported separately.
- `new` and `missing` mean a row is present on only one side. Changes to or from an explicit `excluded` status are reported separately. The runner does not emit `excluded`; the comparator never infers it from an absent row or from `unsupported`.
- `lostCoverage` counts non-excluded baseline rows that are missing or excluded in the candidate. These are coverage losses, not matched-variant regressions.

Exit code `0` means the comparison found neither a regression nor a coverage loss. Exit code `1` means it found either; both codes write the JSON delta to stdout. Exit code `2` means invalid arguments, an unreadable or invalid report, or a scope mismatch; the diagnostic goes to stderr and no delta is written.

Run the comparator fixtures offline without installing dependencies or downloading Test262:

```powershell
node --test tools/test262/delta.test.mjs
```

Run all runner and comparator tests after installing the runner's Node dependencies:

```powershell
npm --prefix tools/test262 test
```
