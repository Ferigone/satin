# Legacy browser Test262 results for alpha.38

The March 23, 2016 Test262 website lists **16,436 cases** across its harness, language, Annex B and built-ins chapters. Running every case in a fresh Satin JavaScript VM produced **16,152 passes in alpha.38 and 16,057 in alpha.37**. All 95 changes from a nonpass to a pass are identifiable by their original Run All index and path. Every case that passed in alpha.37 passed again in this measurement. [Pinned website catalog](https://github.com/v8/test262/blob/e1166500a0d26c8b90e704a31c6646992573f632/website/json/default.json) · [Per-case comparison (TSV.GZ)](legacy-test262-alpha38.tsv.gz)

| Isolated VM on Linux x86_64 | Passed | Other outcomes | Total |
| --- | ---: | ---: | ---: |
| Alpha.37 | 16,057 | 379 | 16,436 |
| Alpha.38 | 16,152 | 284 | 16,436 |

Of the 95 additional passes, 46 are Array cases, 21 Function, 12 language expressions, 6 Object, 3 RegExp, 3 String, 2 arguments-object and 2 ThrowTypeError. In the alpha.38 run, the original website validator reported a failure in 191 cases. The VM reached a resource limit before a verdict in the other 93. These included 28 instruction-budget stops, 33 native-work budget stops, 28 nested bytecode depth stops, three aggregate heap allocation stops and one expression nesting stop. The alpha.37 report recorded 376 failures and three harness errors caused by heap limits. All of them count toward the 379 nonpasses. [Measured alpha.38 engine](https://github.com/Ferigone/satin/tree/d88d5071e0af407761d0e5a7864ebeae6d2efad6/crates/javascript)

An independent Chromium 151 run used the original browser runner in selections with a watchdog. It produced 16,101 passes, 331 failures and four cases with no verdict. All 191 cases that reached a failing assertion in the alpha.38 isolated VM also failed in Chromium. Of the 93 cases that stopped at a VM limit, 61 passed in Chromium, 28 failed there and four returned no verdict. The unmodified Chromium Run All itself stalled at case 1,680.

With the explicit `web-vm-alpha37` diagnostic profile and 100,000,000 instructions per script, the alpha.38 worker passed 16,210. That run used a 512 MiB heap and a 100-second external timeout. The same 191 cases reached a failed assertion and 35 stopped at a VM limit. All 58 changes were improvements. Of the cases Chromium passed, three still stopped in this diagnostic: the 32-deep function parser fixture and the two URI decoders that each test over a million UTF-16 pairs. The indexed comparison records the Chromium result and the diagnostic worker result for every case. [Chromium and diagnostic VM outcomes (TSV.GZ)](legacy-test262-reference-alpha38.tsv.gz)

## What the browser reported

A native alpha.37 window ran cases **4,110–4,112** from a three-case local catalog. The original Run All and Results controls showed **3 run, 2 passed, 1 failed, 0 failed to load**. The website callback identified case **4,111**, `language/expressions/template-literal/tv-null-character-escape-sequence.js`. The fixture contains a literal NUL byte inside its expected string. The site writes the source into an iframe's HTML `<script>` element. HTML parsing changes that byte to U+FFFD. The template escape still evaluates to U+0000, and the assertion fails. Chromium reported the same two characters. Both Satin and Node passed the fixture as standalone JavaScript. [2016 fixture](https://github.com/v8/test262/blob/e1166500a0d26c8b90e704a31c6646992573f632/test/language/expressions/template-literal/tv-null-character-escape-sequence.js) · [Website iframe runner](https://github.com/v8/test262/blob/e1166500a0d26c8b90e704a31c6646992573f632/website/scripts/sth.js) · [HTML script data state](https://html.spec.whatwg.org/multipage/parsing.html#script-data-state)

At the reported **8,276 run / 224 failed** checkpoint, the alpha.37 isolated VM had 8,053 passes and 223 nonpasses. Case 4,111 accounts for the additional browser failure. An independent stock browser run using the original controller and fresh browser processes for short batches covered all **3,087 consecutive cases from 5,190 through 8,276**. It recorded **3,012 passes and 75 failures**. The 75 matched 72 VM failures and all three VM harness errors. They included 16 resource-limited `decodeURI` cases and 11 resource-limited RegExp cases. These observations cover the indicated prefix or batches. A full native result requires the website's completion and Results counters for all 16,436 cases.

Case **1,680**, `language/statements/for-of/iterator-next-reference.js`, illustrates a change in ECMAScript. The 2016 fixture replaces an iterator's `next` method during the first iteration and expects the replacement to end the loop. A modern engine caches the original method, so the legacy loop continues. The isolated Satin VM reached its instruction budget and a Chromium website run produced no verdict before its external deadline. The pinned modern test at the same path checks that the engine reads `next` exactly once. [2016 fixture](https://github.com/v8/test262/blob/e1166500a0d26c8b90e704a31c6646992573f632/test/language/statements/for-of/iterator-next-reference.js) · [Modern fixture](https://github.com/tc39/test262/blob/48221d7c1f7745dc13e420a2e850a248c0b2afb4/test/language/statements/for-of/iterator-next-reference.js)

## Reproduce the two measurements

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

To reproduce the website measurement, serve the pinned `website` directory and leave the server running. In a second terminal, supply the path to a native Satin executable. The probe opens a window, clicks Run All, waits for completion and checks the Results counters. Add `--require-all-pass` to require all reported cases to pass. [Native website procedure](../../tools/test262/website.md)

```sh
python3 -m http.server 8000 --bind 127.0.0.1 --directory "$run_dir/test262/website"
```

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
