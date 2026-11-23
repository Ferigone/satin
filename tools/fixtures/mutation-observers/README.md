# MutationObserver fixtures

The shared streaming fixture is `index.html`. The verifier sends the first
part, waits for the prefix script's release request, and sends the rest.
The page checks record payloads, removed-subtree observation, and callback
ordering after a click.

The `wpt/` directory contains eight original test files, one support script,
and the license from the pinned upstream revision. The verifier wraps the
two `.window.js` files in a driver page and serves their original bytes.
The local `testharness.js` adapter provides the APIs used by the specimens,
including timer steps and array assertions.
[WPT revision](https://github.com/web-platform-tests/wpt/tree/fa649586f35119af9032312ef21e300e41d1c3b6/dom/nodes)
