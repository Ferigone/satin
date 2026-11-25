# Legacy browser Test262 results

## Full Run All in alpha.40

Satin’s browser engine loaded all four chapters of the pinned March 2016 website from a local HTTP server. The original controller received one result for each of the 16,436 cases, in catalog order. It called its completion hook after 26 minutes and 10 seconds, with no timer left pending. [Original website](https://github.com/v8/test262/tree/e1166500a0d26c8b90e704a31c6646992573f632/website) · [Measured source](https://github.com/Ferigone/satin/tree/fc9de7072128f91df6b509f28dee3c7c3d0f7cb2)

| Linux x86_64 browser engine | Ran | Passed | Failed | Failed to load |
| --- | ---: | ---: | ---: | ---: |
| At `Set/prototype/size/size.js` | 8,276 | 8,088 | 188 | 0 |
| On the website’s completion callback | 16,436 | 16,194 | 242 | 0 |

The driver exercised the Run, Run All and Results controls using native input dispatch in the browser engine. After completion, Results displayed the same run, pass and fail totals, with 4,911 rows in its top-level table and 4,826 source links. Opening Results added zero script diagnostics. The 43 cases that changed since the alpha.38 engine run all moved from fail to pass. Each of these also passed in the independent Chromium 151 reference. Among the remaining 242, Chromium passed 18, failed 220 and returned no verdict for four. The two fixtures discussed below show how the historical site’s HTML and JavaScript expectations affect the counts. [Indexed failures](legacy-test262-controller-failures-alpha40.jsonl.gz) · [Counts and comparisons](legacy-test262-controller-alpha40.json) · [Measurement driver](fixtures/legacy-test262-controller-alpha40.rs)

Of the 242 failed callbacks, 50 explicitly reported a VM limit. These included 28 bytecode-depth limits, nine instruction limits, five native-work limits, four child CPU timeouts, three heap limits and one expression nesting limit. The remaining 192 also failed in Chromium. Of the 43 new passes, 36 exercise URI encoding or decoding, five exercise RegExp, one exercises a large array and one checks JavaScript comments.

For an independent repeat, serve the pinned website at `http://127.0.0.1:8000/default.html` using the server instructions below. Run these commands from a Satin checkout containing this report. The temporary worktree keeps the compiler inputs pinned while the indexer compares the callbacks against the checked-in catalogs.

```sh
satin_checkout=$(pwd)
run_dir=$(mktemp -d)
git worktree add --detach "$run_dir/satin" fc9de7072128f91df6b509f28dee3c7c3d0f7cb2
mkdir -p "$run_dir/satin/crates/engine/examples" "$run_dir/observed"
cp docs/compatibility/fixtures/legacy-test262-controller-alpha40.rs \
  "$run_dir/satin/crates/engine/examples/legacy_test262_controller.rs"
cargo run --release --locked --manifest-path "$run_dir/satin/Cargo.toml" \
  -p rust-browser-engine --example legacy_test262_controller -- \
  http://127.0.0.1:8000/default.html "$run_dir/observed" 2> "$run_dir/observed/progress.log"
python3 "$satin_checkout/tools/test262/legacy-controller-index.py" \
  "$run_dir/observed" "$run_dir/alpha40-failures.jsonl.gz"
```

## Earlier browser and standalone measurements

The March 23, 2016 Test262 website lists **16,436 cases** across its harness, language, Annex B and built-ins chapters. One alpha.38 browser-engine session completed all four chapters with **16,151 passed, 285 failed and 0 failed to load**. Running every case in a fresh Satin JavaScript VM produced **16,152 passes in alpha.38 and 16,057 in alpha.37**. All 95 changes from a nonpass to a pass are identifiable by their original Run All index and path. Every case that passed in alpha.37 passed again in this measurement. [Pinned website catalog](https://github.com/v8/test262/blob/e1166500a0d26c8b90e704a31c6646992573f632/website/json/default.json) · [Per-case comparison (TSV.GZ)](legacy-test262-alpha38.tsv.gz)

| Isolated VM on Linux x86_64 | Passed | Other outcomes | Total |
| --- | ---: | ---: | ---: |
| Alpha.37 | 16,057 | 379 | 16,436 |
| Alpha.38 | 16,152 | 284 | 16,436 |

Of the 95 additional passes, 46 are Array cases, 21 Function, 12 language expressions, 6 Object, 3 RegExp, 3 String, 2 arguments-object and 2 ThrowTypeError. In the alpha.38 run, the original website validator reported a failure in 191 cases. The VM reached a resource limit before a verdict in the other 93. These included 28 instruction-budget stops, 33 native-work budget stops, 28 nested bytecode depth stops, three aggregate heap allocation stops and one expression nesting stop. The alpha.37 report recorded 376 failures and three harness errors caused by heap limits. All of them count toward the 379 nonpasses. [Measured alpha.38 engine](https://github.com/Ferigone/satin/tree/d88d5071e0af407761d0e5a7864ebeae6d2efad6/crates/javascript)

An independent Chromium 151 run used the original browser runner in selections with a watchdog. It produced 16,101 passes, 331 failures and four cases with no verdict. All 191 cases that reached a failing assertion in the alpha.38 isolated VM also failed in Chromium. Of the 93 cases that stopped at a VM limit, 61 passed in Chromium, 28 failed there and four returned no verdict. The unmodified Chromium Run All itself stalled at case 1,680.

With the explicit `web-vm-alpha37` diagnostic profile and 100,000,000 instructions per script, the alpha.38 worker passed 16,210. That run used a 512 MiB heap and a 100-second external timeout. The same 191 cases reached a failed assertion and 35 stopped at a VM limit. All 58 changes were improvements. Of the cases Chromium passed, three still stopped in this diagnostic: the 32-deep function parser fixture and the two URI decoders that each test over a million UTF-16 pairs. The indexed comparison records the Chromium result and the diagnostic worker result for every case. [Chromium and diagnostic VM outcomes (TSV.GZ)](legacy-test262-reference-alpha38.tsv.gz)

## Full Windows run in alpha.39

On Windows Server 2022, the native Satin window completed the original Run All and displayed **16,436 run, 16,151 passed, 285 failed and zero failed to load**. Its Run and Results counters agreed. The Run tab’s failure log held 285 rows. The separate table on the Results tab was empty. The measured build at `f6efbb39` and the public alpha.39 tag differ only in the README. [Windows recording and checkpoints](https://github.com/Ferigone/satin/actions/runs/45660908569/artifacts/15048006166) · [Source comparison](https://github.com/Ferigone/satin/compare/f6efbb39c94bbe654ccf3cd5728b9a57e946db81...147279aa93199d55106410f1cb7490da91e0dce2)

## Native Results in alpha.40

The alpha.39 native window reproduced an empty results table with 16,436 preloaded case records. Before the browser had inserted the table, the JavaScript VM reached its 512 MiB heap limit while constructing the HTML for row 3,047. At that point it still charged about 400 MB for temporary strings that the next collection discarded. Alpha.40 releases a large string’s reservation as soon as its last reference disappears. An iframe also inherits the allowance of the timer or input handler that executed it, while the parent keeps one quarter of the remaining instructions and CPU time to handle errors. [Measured engine source](https://github.com/Ferigone/satin/commit/fc9de7072128f91df6b509f28dee3c7c3d0f7cb2) · [String accounting regressions](../../crates/javascript/tests/native_copy_budget.rs)

In a Linux desktop window, the original Results renderer populated **4,911 table rows** from the same preloaded data with no script, layout or paint limit. Two independent browser-engine sessions each opened the table twice and bound a click handler to every one of its **4,826 source links**. These counts describe the table shown at the top of the site’s section hierarchy. All 16,436 tests remain in the four original catalog files. The preloaded-data replay measures report rendering only.

## What the browser reported

A Linux x86_64 headless driver loaded the original website over local HTTP into Satin’s release browser engine. The website’s **Run All** controller executed all 16,436 cases in their original order in one document session, including its timers, Base64 decoder and iframe scripts. At case 8,276 the site had reported **8,069 passes and 207 failures**. After 34 minutes and 6 seconds the controller called its completion hook. Its 16,436 per-test callbacks agreed with the final Run-tab counters, with **16,151 passes, 285 failures and zero failed loads**. The event loop had no pending timer. [Pinned website and controller](https://github.com/v8/test262/tree/e1166500a0d26c8b90e704a31c6646992573f632/website) · [Measurement driver](fixtures/legacy-test262-controller-alpha38.rs) · [All 285 browser failures (JSONL.GZ)](legacy-test262-controller-failures-alpha38.jsonl.gz)

The driver used Satin’s `Loader` and `ScriptRuntime`, drove the page’s Run and Run All controls through jQuery, and observed results with the website’s implementer hook. The build used Git tree `9e017356490257db5993b1eee076b4bedfcf385d`, identical to the entire source tree at public commit `d88d5071`. The 74 files in the HTTP mirror matched the corresponding upstream Git blobs. This measurement covers the browser engine without an operating-system window. A separate attempt to open the Results tab through the driver after completion hit the alpha.38 native-work budget while building the report. The recorded counts came from the Run tab and callbacks. [Public Satin source](https://github.com/Ferigone/satin/tree/d88d5071e0af407761d0e5a7864ebeae6d2efad6)

All 284 isolated VM nonpasses also failed in the browser. Case 4,111 was the only additional failure. Among the 285 browser failures, the Chromium reference recorded 220 failures, four cases without a verdict and 61 passes. Of the 16,151 cases Satin passed, Chromium passed 16,040 and failed 111. [Per-case Chromium reference](legacy-test262-reference-alpha38.tsv.gz)

A native alpha.37 window ran cases **4,110–4,112** from a three-case local catalog. The original Run All and Results controls showed **3 run, 2 passed, 1 failed, 0 failed to load**. The website callback identified case **4,111**, `language/expressions/template-literal/tv-null-character-escape-sequence.js`. The fixture contains a literal NUL byte inside its expected string. The site writes the source into an iframe's HTML `<script>` element. HTML parsing changes that byte to U+FFFD. The template escape still evaluates to U+0000, and the assertion fails. Chromium reported the same two characters. Both Satin and Node passed the fixture as standalone JavaScript. [2016 fixture](https://github.com/v8/test262/blob/e1166500a0d26c8b90e704a31c6646992573f632/test/language/expressions/template-literal/tv-null-character-escape-sequence.js) · [Website iframe runner](https://github.com/v8/test262/blob/e1166500a0d26c8b90e704a31c6646992573f632/website/scripts/sth.js) · [HTML script data state](https://html.spec.whatwg.org/multipage/parsing.html#script-data-state)

At the reported **8,276 run / 224 failed** checkpoint, the alpha.37 isolated VM had 8,053 passes and 223 nonpasses. Case 4,111 accounts for the additional browser failure. An independent stock browser run using the original controller and fresh browser processes for short batches covered all **3,087 consecutive cases from 5,190 through 8,276**. It recorded **3,012 passes and 75 failures**. The 75 matched 72 VM failures and all three VM harness errors. They included 16 resource-limited `decodeURI` cases and 11 resource-limited RegExp cases. The alpha.37 window and batch checks cover those selections. The alpha.38 browser-engine measurement above continued through the last catalog entry.

Case **1,680**, `language/statements/for-of/iterator-next-reference.js`, illustrates a change in ECMAScript. The 2016 fixture replaces an iterator's `next` method during the first iteration and expects the replacement to end the loop. A modern engine caches the original method, so the legacy loop continues. The isolated Satin VM reached its instruction budget and a Chromium website run produced no verdict before its external deadline. The pinned modern test at the same path checks that the engine reads `next` exactly once. [2016 fixture](https://github.com/v8/test262/blob/e1166500a0d26c8b90e704a31c6646992573f632/test/language/statements/for-of/iterator-next-reference.js) · [Modern fixture](https://github.com/tc39/test262/blob/48221d7c1f7745dc13e420a2e850a248c0b2afb4/test/language/statements/for-of/iterator-next-reference.js)

## Reproduce the measurements

From a Satin checkout containing alpha.38, run the following on Linux with the pinned Rust toolchain, Python 3 and Git. The legacy worker adapter first shipped in alpha.38. The commands copy its two source files onto a worktree containing the alpha.37 JavaScript engine. The runner checks the upstream Git blob IDs of the four catalogs and the helper scripts. [Alpha.37 engine](https://github.com/Ferigone/satin/tree/014e7754608bb500ba2c32b80af1094793ae2e0b/crates/javascript) · [Worker adapter](https://github.com/Ferigone/satin/blob/d88d5071e0af407761d0e5a7864ebeae6d2efad6/crates/js-harness/src/legacy.rs)

```sh
set -eu
run_dir=$(mktemp -d)
git init -q "$run_dir/test262"
git -C "$run_dir/test262" fetch --depth=1 https://github.com/v8/test262.git e1166500a0d26c8b90e704a31c6646992573f632
git -C "$run_dir/test262" checkout --detach FETCH_HEAD
git worktree add --detach "$run_dir/alpha37" 014e7754608bb500ba2c32b80af1094793ae2e0b
git -C "$run_dir/alpha37" restore --source=d88d5071e0af407761d0e5a7864ebeae6d2efad6 -- \
  crates/js-harness/src/main.rs crates/js-harness/src/legacy.rs
git worktree add --detach "$run_dir/alpha38" d88d5071e0af407761d0e5a7864ebeae6d2efad6
for version in alpha37 alpha38
do
  cargo build --release --locked -p rust-browser-js-harness \
    --manifest-path "$run_dir/$version/Cargo.toml" --target-dir "$run_dir/build-$version"
  python3 tools/test262/legacy-vm.py --website "$run_dir/test262/website" \
    --worker "$run_dir/build-$version/release/rb-js" --output "$run_dir/$version.jsonl"
done
```

The measured Linux x86_64 release worker had SHA-256 `c5996f41efa2d5829c524c68ca43bf373c40d7f81ee47f83608e1a8d14ffa088`. The default worker permits 1,000,000 instructions per script, 100,000 heap objects and a 64 MiB aggregate heap allocation budget. The website runs child scripts with iframe globals on the document VM's shared heap. The document profile permits 524,288 heap objects and 512 MiB, and charges bulk buffer work in groups of 1,024 units per instruction. Alpha.38 applies a separate 24,000,000-instruction and two-second CPU budget to a timer task. To inspect a case under a different isolated budget, use the versioned diagnostic profile in the runner guide. [VM defaults](https://github.com/Ferigone/satin/blob/d88d5071e0af407761d0e5a7864ebeae6d2efad6/crates/javascript/src/vm.rs) · [Document limits](https://github.com/Ferigone/satin/blob/d88d5071e0af407761d0e5a7864ebeae6d2efad6/crates/engine/src/script.rs) · [Timer limits](https://github.com/Ferigone/satin/blob/d88d5071e0af407761d0e5a7864ebeae6d2efad6/crates/engine/src/script/scheduler.rs) · [Diagnostic profile](../../tools/test262/website.md#isolated-vm-resource-diagnostics)

Serve the same pinned website on loopback and leave the server running while the browser engine or desktop window uses it.

```sh
python3 -m http.server 8000 --bind 127.0.0.1 --directory "$run_dir/test262/website"
```

To reproduce the single-session engine result, use the alpha.38 worktree prepared above. From a second terminal with the same `run_dir` value, copy the published measurement driver into that temporary worktree. The driver writes one JSON array per failed callback with the one-based Run All index, exact catalog path, website verdict and error text. Its final diagnostic line records the completion hook and Run-tab counters. The archived file has zero gzip timestamp and no stored filename. Its **uncompressed** SHA-256 is `7b8eca5eddcefdc2f5d7518a07b1e069ef4aadf9b3e33113364898e55630cb68`. The indexes and paths can also be joined to the 16,436-row isolated VM and Chromium archives. [Measurement driver](fixtures/legacy-test262-controller-alpha38.rs) · [Recorded callbacks](legacy-test262-controller-failures-alpha38.jsonl.gz)

```sh
cp docs/compatibility/fixtures/legacy-test262-controller-alpha38.rs \
  "$run_dir/alpha38/crates/engine/examples/legacy_test262_controller.rs"
cargo run --locked --release --manifest-path "$run_dir/alpha38/Cargo.toml" \
  -p rust-browser-engine --example legacy_test262_controller -- \
  http://127.0.0.1:8000/default.html "$run_dir/browser-failures.jsonl"
sha256sum "$run_dir/browser-failures.jsonl"
gzip -dc docs/compatibility/legacy-test262-controller-failures-alpha38.jsonl.gz | sha256sum
```

For an operating-system window, the native probe clicks Run All, waits for completion and checks the Results counters. Add `--require-all-pass` to require all reported cases to pass. [Native website procedure](../../tools/test262/website.md)

```sh
node tools/test262/website.mjs --satin /path/to/rust-browser --native --until complete \
  --site-url http://127.0.0.1:8000/default.html --output legacy-native
```

## Verify the per-case comparison

The gzip contains a UTF-8 TSV header and 16,436 rows in the original website order. In the alpha.37 column, `nonpass` includes both failures and the three confirmed heap errors. In alpha.38, `pass` and `fail` record the original validator's verdict and `vm_limit` records a resource stop. The uncompressed SHA-256 is `6ba34c13c44940be232d99e8ccf41e6d77711bdb8da10d8528c631bd3df6989b`.

Regenerate the archive from the two JSONL reports in the same shell. The gzip header uses an empty filename and zero timestamp.

```sh
python3 tools/test262/legacy-compare.py --baseline "$run_dir/alpha37.jsonl" \
  --candidate "$run_dir/alpha38.jsonl" --output "$run_dir/comparison.tsv.gz"
gzip -dc "$run_dir/comparison.tsv.gz" | sha256sum
```
