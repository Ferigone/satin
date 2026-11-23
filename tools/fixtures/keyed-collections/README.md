# Keyed collections verification

The loopback fixture exercises `Map`, `Set`, `WeakMap` and `WeakSet` in
a browser document. Its receipt covers key identity, signed zero, live
iteration, iterator closing, grouping, reentrant insertions, set operations
and object and symbol weak keys.

```powershell
node tools/verify-keyed-collections.mjs target/release/rust-browser.exe target/keyed-collections-verification
node tools/verify-keyed-collections.mjs target/release/rust-browser.exe target/keyed-collections-verification --chrome-reference --rea PATH_TO_REA_MJS
```

`REA_CLI` can supply the REA path. Use `--chrome` for a Chrome executable
outside the default Windows installation. `--chrome-reference-only` runs
the fixture in an isolated Chrome without launching the Rust executable.
The runner saves the executable and fixture checksums, final DOM and process
results to the selected output directory.

The Rust GC tests exercise reachability directly, including conditional
values, dead key–value cycles and symbols kept alive by referenced objects
or queued jobs.

```powershell
cargo test -p rust-browser-javascript --test keyed_collections
```

The Test262 command includes the six complete groups at revision
`48221d7c1f7745dc13e420a2e850a248c0b2afb4`. The runner reads the original
scripts and harness from verified Git blobs, including on an EOL-converting
Windows checkout. [Test262 keyed collections at the pinned revision](https://github.com/tc39/test262/tree/48221d7c1f7745dc13e420a2e850a248c0b2afb4/test/built-ins)

```powershell
node tools/test262/runner.mjs --test262 PATH_TO_TEST262 --worker target/release/rb-js.exe --path built-ins/Map --path built-ins/Set --path built-ins/WeakMap --path built-ins/WeakSet --path built-ins/MapIteratorPrototype --path built-ins/SetIteratorPrototype --output target/keyed-collections-test262.json
```
