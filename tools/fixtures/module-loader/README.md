# Module loader fixture

Run from the repository root with Node.js 22+ and local Windows browser binaries:

```powershell
node tools/verify-module-loader.mjs --chrome "C:\Program Files\Google\Chrome\Application\chrome.exe" --satin "C:\path\to\rust-browser.exe"
```

Either browser flag can be omitted. The verifier hosts the same fixture bytes at
separate paths on a single loopback-only HTTP server, with a fresh profile and
`Cache-Control: no-store` for each run. It does not need an external website,
package install, or a browser already running. Reports default to a new
`target/module-loader-*` directory; use `--output` to choose a path. The report
contains per-assertion results, browser versions and hashes, the fixture hashes,
the DOM receipt, and the server request log. Full HTML and process logs are
saved alongside it. Any failed assertion exits with status 1.

The fixture asserts:

- The deep scoped importer uses the deepest `flavor` mapping, falls back to the
  enclosing scope for `outer-only`, and falls back to the top-level prefix
  mapping for `global/global-fallback.mjs`. An importer outside the scopes uses
  the top-level `flavor`.
- `a.mjs` calls `import()` with a bare alias while `b.mjs` supplies a referrer-
  relative URL to the same module. Both cold calls start in one job, and a third
  call runs after both resolve. The cold promises are distinct, but all three
  results have identical namespace and exported object identities. The shared
  module evaluates once, is fetched once, and a mutation through one namespace
  updates the live binding on the others.
- An explicit `null` and a non-URL address string in the deep scope each block
  an otherwise valid top-level fallback with a `TypeError`. No request should
  reach `would-be-relative.mjs`, which would load successfully if an engine
  incorrectly parsed the bare address as document-relative. This follows
  [sorting and normalizing a module specifier map][normalize] in the HTML Standard.

[normalize]: https://html.spec.whatwg.org/multipage/webappapis.html#sorting-and-normalizing-a-module-specifier-map
