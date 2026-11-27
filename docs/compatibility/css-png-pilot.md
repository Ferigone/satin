# Running the replaced-PNG pilot

From the repository root:

```text
cargo build --release -p rust-browser-engine --example css_reftest_render
python tools/css_png_pilot.py WPT_ROOT MANIFEST target/release/examples/css_reftest_render.exe NEW_REPORT.json
```

Omit `.exe` on Unix. The runner passes `--local-png ABSOLUTE_IMAGE_URL` to the example worker after the private staging root. The URL is a lookup key, never fetched. Only the verified `binding.png` is decoded: one 304-byte pinned input, 4 KiB worker input cap, 100×100 maximum dimensions and 1 MiB decoder allocation limit. Only the pinned manifest, test, reference and asset are eligible. Both HTML inputs are capped at 4 KiB. Redirected paths, server headers, alternative source URLs, and extra CSS dependencies are rejected.

The first worker must return the exact nonuniform 800×600 checkerboard control fingerprint. Preflight, timeout or fingerprint failure on that control aborts scoring. A second worker renders the real `!=` pair, under a separate timeout (30 seconds per invocation by default, configurable with `--timeout`). Identical buffers fail. Different buffers pass only when both are nonuniform; otherwise the mismatch is inconclusive.

The new report counts PASS, FAIL, ERROR, TIMEOUT, SKIP and INCONCLUSIVE over **one** admitted row. It reports the other **26,317 of 26,318** pinned screen entries separately, not as pilot SKIPs. The control is not a WPT row. Neither the static nor the linked-Ahem report or denominator changes.
