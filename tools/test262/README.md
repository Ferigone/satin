# Test262 runner

The runner sends test source to `rb-js`, the Satin JavaScript engine's test worker. Node handles YAML metadata, file selection, and process supervision. The worker runs each variant in a fresh VM.

Use a [Test262 checkout](https://github.com/tc39/test262) at commit `48221d7c1f7745dc13e420a2e850a248c0b2afb4`. The runner reads source and harness bytes from Git objects and verifies blob hashes. Working-tree line endings leave the parser input unchanged.

From the repository root, run

```powershell
cargo build -p rust-browser-js-harness --release --locked
npm --prefix tools/test262 ci --ignore-scripts --no-audit --no-fund
node tools/test262/runner.mjs --test262 PATH_TO_TEST262 --worker target/release/rb-js.exe --path language/expressions/addition --path language/expressions/multiplication --path language/expressions/subtraction --path language/expressions/strict-equals --path language/statements/if --output target/test262/report.json
```

The runner prepares sloppy and strict variants according to the `noStrict`, `onlyStrict`, `raw`, and `module` flags. Non-raw variants load upstream `sta.js`, `assert.js`, and the requested `includes`. Non-raw `async` variants also load upstream `doneprintHandle.js`. Parse and early errors are checked before execution. The harness must succeed before a negative runtime test can count its expected error.

## Modules

Module variants use the separate strict module grammar. The parser supplies import requests in source order with their exact UTF-16 units. The runner reads relative source files from pinned Git blobs and sends a dependency graph to the worker. The VM links and evaluates live bindings, namespace objects, cycles, and `import.meta`. Harness files run as separate scripts in the same VM.

Negative resolution tests require the expected error class during linking. Host setup, missing fixtures, and entry syntax errors get separate outcomes. Negative runtime tests execute the linked graph. Top-level await suspends module execution until the relevant Promise jobs run. A graph still suspended after the queue drains receives `async_incomplete`.

Dynamic import and top-level await cases supply the original relative `_FIXTURE.js` files. The worker resolves a dynamic request when it runs and shares the module record for each file. A dynamic fixture set is limited to 1,023 files and 4 MiB of source. A static graph is limited to 1,023 fixtures, 16,384 edges, and 8 MiB of source. Source-phase and deferred imports, import attributes, JSON modules, external URLs, and agent hosts remain unsupported.

## Asynchronous tests

Upstream `doneprintHandle.js` supplies `$DONE` to asynchronous tests. The worker drains a shared FIFO queue for Promise reactions, thenable assimilation, queued microtasks, and async function resumes. Source and checkpoint share one execution budget.

The worker observes the first argument to `print` as required by Test262. Passing requires one `Test262:AsyncTestComplete` and zero `Test262:AsyncTestFailure:` markers. Extra completions or failure markers fail the test, even after the diagnostic log fills. An empty queue without a completion marker yields `async_incomplete`.

After the checkpoint, the worker checks unhandled rejections against the rooted original `Test262Error` prototype chain. In an async test, an unhandled rejection with that prototype fails the test, including one that follows `$DONE()`. Synchronous variants record all unhandled rejections as diagnostics. Async variants record other rejection reasons as diagnostics. A handler added before the checkpoint removes the source Promise from the unhandled list.

Timers and external worker or agent hosts are unsupported. Unsupported language constructs receive `unsupported`. An upstream harness execution error receives `harness_error`.

## Reports and limits

The report records the pinned revision, worker SHA-256, selection, results per variant, and resource limits. Pass rates apply to the selected paths. Exit code 1 indicates at least one non-passing result. Exit code 2 indicates a configuration or runner failure.

Repeat `--path` to select paths relative to the Test262 `test` directory. Use `--limit` and `--offset` to select a subset. The default `--timeout-ms` is 3,000 and the default `--instructions` is 1,000,000. Fixture files are excluded from the test selection.

The runner sends at most one request at a time. JSONL requests are capped at 16 MiB. The worker bounds the execution budget to 1–100,000,000 instructions, charges native work and string copies, and limits each checkpoint to 10,000 jobs. The VM queue defaults to 4,096 jobs. Reaction and unhandled-rejection limits also apply. The log holds up to 64 entries, each with up to 2,048 UTF-16 units of preview.

The VM defaults to 64 MiB of estimated allocations and 100,000 heap cells, with additional source, text, and nesting limits. The allocation budget estimates VM memory. Reaching a resource limit yields `resource_limit`. The outer deadline stops only the worker process created by the runner and records `timeout` or `crash` as appropriate. [JavaScript engine documentation](../../docs/JAVASCRIPT.md)

Run the runner's classification tests with

```powershell
npm --prefix tools/test262 test
```
