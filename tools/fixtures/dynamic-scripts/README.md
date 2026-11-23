# Dynamic script verification fixtures

The loopback server in `tools/verify-dynamic-scripts.mjs` serves these fixtures
to Rust Browser and an owned Chrome process. It supplies a second origin for
module CORS checks and holds responses at named barriers for reproducible
execution order.

The HTML and JavaScript inside `wpt/` are byte-exact Web Platform Tests
specimens from revision `fa649586f35119af9032312ef21e300e41d1c3b6`.
The verifier checks each original file's SHA-256 and reports all registered
subtests. `testharness.js` in this directory is the local adapter used to
report those results. The pinned upstream license is in `wpt/LICENSE.md`.
[Upstream tests and support resources](https://github.com/web-platform-tests/wpt/tree/fa649586f35119af9032312ef21e300e41d1c3b6/html/semantics/scripting-1/the-script-element)
