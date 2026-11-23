# Pinned URL Web Platform Tests

The selection contains every `urlsearchparams-*.any.js` file in the pinned
revision, the live link between `URL` and its search parameters, and both URL
static parsing files. The upstream scripts and license retain their original
bytes. The manifest records their SHA-256 checksums, Git blob identities, and
subtest counts. [WPT URL suite at 9e1907a](https://github.com/web-platform-tests/wpt/tree/9e1907a13cbbc338975302097a4bafcbc4d12dc5/url)

```powershell
cargo test -p rust-browser-engine --test wpt_url -- --nocapture
```

The test verifies each fixture's checksum and runs each complete script in a
fresh Rust Browser document realm through `ScriptRuntime`. The bridge
implements the synchronous assertion functions that these files call. It
leaves the page APIs and original assertions intact. The runner prints
individual file totals, any failed subtest names, and any script-level error.
[WPT testharness API](https://web-platform-tests.org/writing-tests/testharness-api.html)

The constructor file contains a generator in its final test callback. The
engine rejects that syntax while compiling the complete script, so all 27
constructor subtests are reported as unexecuted. The runner requires that
specific failure at its pinned location and verifies the other files against
their full subtest counts. A separate diagnostic run of the constructor
prefix found two other gaps involving legacy DOMException constants and
FormData. The prefix result is excluded from the pinned-file totals.

The reference run used the official testharness script from the same WPT
revision with isolated document realms in Chrome 157.0.8103.51. REA started
and terminated the owned Chrome process. Two subtests failed because Chrome
kept a raw final space when removing an opaque path's query.
The pinned suite expects the final space to be percent encoded.
[WHATWG opaque path state](https://url.spec.whatwg.org/#opaque-path-state)

| WPT file | Rust passed | Rust unexecuted | Chrome passed | Chrome failed |
| --- | ---: | ---: | ---: | ---: |
| `url-searchparams.any.js` | 4 | 0 | 4 | 0 |
| `url-statics-canparse.any.js` | 8 | 0 | 8 | 0 |
| `url-statics-parse.any.js` | 8 | 0 | 8 | 0 |
| `urlsearchparams-append.any.js` | 4 | 0 | 4 | 0 |
| `urlsearchparams-constructor.any.js` | 0 | 27 | 27 | 0 |
| `urlsearchparams-delete.any.js` | 8 | 0 | 6 | 2 |
| `urlsearchparams-foreach.any.js` | 6 | 0 | 6 | 0 |
| `urlsearchparams-get.any.js` | 2 | 0 | 2 | 0 |
| `urlsearchparams-getall.any.js` | 2 | 0 | 2 | 0 |
| `urlsearchparams-has.any.js` | 4 | 0 | 4 | 0 |
| `urlsearchparams-set.any.js` | 2 | 0 | 2 | 0 |
| `urlsearchparams-size.any.js` | 4 | 0 | 4 | 0 |
| `urlsearchparams-sort.any.js` | 17 | 0 | 17 | 0 |
| `urlsearchparams-stringifier.any.js` | 14 | 0 | 14 | 0 |
| **Total** | **83** | **27** | **108** | **2** |

Rust had zero failed subtests. Chrome executed all 110 subtests.
The REA evidence ID is
`ev_10a4cbd4664111364b5fc484b7e3d72942b9ad29a4c44877efd63d365f7f48e8`.
