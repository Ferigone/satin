# Running the legacy Test262 website

Satin can run a chapter or the entire suite through the controls on the [original Test262 website](https://v8.github.io/test262/website/default.html). This is the legacy browser suite. The [modern Test262 runner](README.md) uses the pinned upstream JavaScript tests and a standalone Satin VM.

If Satin is already installed, open the website, select **Run**, then click **Run** in the row for the chapter you want or **Run All** above the table. Open **Results** when testing finishes, or click **Pause** first to see results so far.

## Run a chapter from the command line

Build the native browser, then run the website probe from the repository root with Node 22.15 or newer. These PowerShell examples use a Windows release build. Linux uses `target/release/rust-browser` and needs a display for the native window.

```powershell
cargo build --release --locked -p rust-browser
node tools/test262/website.mjs --satin target/release/rust-browser.exe --chapter json/language.json
```

The site’s default catalog lists `harness`, `language`, `annexB` and `built-ins`. Supply the chapter name, `json/NAME` or `json/NAME.json`. The probe opens Run, waits for the matching chapter row to load and clicks its **Run** image. The browser locates the image inside the row and hit-tests it before sending the click. The chapter’s advertised count comes from that run.

| Goal | Arguments after `node tools/test262/website.mjs --satin target/release/rust-browser.exe` |
| --- | --- |
| Complete the short harness chapter | `--chapter harness` |
| Complete the language chapter | `--chapter json/language.json` |
| Complete the built-in object chapter | `--chapter built-ins` |
| Complete Annex B | `--chapter annexB` |
| Complete Run All | `--until complete --timeout-ms 7200000` |
| Require every selected case to pass and return a failing exit code on any failure or incomplete run | `--require-all-pass` |
| Observe Run All making progress, pause and open Results | `--until first` |

`--chapter` defaults to completion. A native run without a chapter defaults to Run All. Use `--until first` for a short progress check. Full runs of the larger chapters and Run All allow six hours by default. First-result checks, harness and Annex B allow seven minutes. Use `--timeout-ms` to set a shorter deadline or allow up to six hours. Execution, startup and Results observation have separate deadlines. The native catalog and row waits are capped at two minutes each. Each invocation creates a fresh browser profile and writes a report, action file, screenshot and checkpoint evidence under `target/test262/website/run-*`. Use `--output DIR` to change the parent directory.

To run an existing HTTP(S) mirror, pass its document URL with `--site-url http://127.0.0.1:8000/default.html`. The report records the actual URL and identifies alternate sites separately from the public endpoint.

## Reading results

Satin records the initial Run catalog, the selected run and the Results pane from one native tab. A completed run requires the site's **Testing complete!** marker, the expected number of executed cases, consistent pass/fail counts, matching suite version/date, and matching Results counters. Each snapshot must share the same capture, tab and document. A failure reported by the test suite counts as a test outcome. The website includes failed-to-load cases in its Fail total.

A first-result capture records the first observed positive count and pauses the browser. More cases can finish before the pause takes effect. The report preserves the first counts and verifies the paused counts against Results. A timeout or invalid capture is `unverified`. Exit 0 means each requested browser reached its observation target, exit 1 means at least one capture was unverified and exit 2 means invalid arguments or a runner failure. The report records separate outcomes if several browsers are selected.

## Chromium reference and headless diagnostics

Add one or more `--chromium FILE` arguments to run the same selection in Chromium. The reference probe uses a fresh browser profile, a local DevTools connection and hit-tested pointer input. For a reproducible short comparison, the public suite version `1234`, dated `2016-03-23`, advertises 55 tests in its harness chapter.

```powershell
node tools/test262/website.mjs --chromium 'C:/Program Files/Google/Chrome/Application/chrome.exe' --chapter harness
```

A plain `--satin FILE` invocation keeps the independent headless load and control diagnostics for investigating a site that fails to initialize. Select `--native`, `--chapter` or `--until` for the native evidence workflow. `--satin-run-ms` controls the headless diagnostic task budget (4,000 ms by default). The separate [native checkpoint verification procedure](native-checkpoint.md) also accepts previously recorded manual harness captures.

```powershell
node --test tools/test262/website.test.mjs tools/test262/native-checkpoint-verify.test.mjs
```

## Reproduce an individual website case in the standalone VM

For a fast engine diagnostic, the legacy VM adapter uses the website's pinned four catalogs, declared includes, error handler and result validator. It runs one classic script per fresh VM, applies the original strict-mode flag and drains Promise jobs for asynchronous cases. Its output records every selected path, verdict and exception. A host resource limit or worker failure is reported explicitly. This adapter has no DOM or real iframe lifecycle. Use the native website procedure above to measure browser progress and verify the displayed score.

```sh
git clone --branch gh-pages https://github.com/v8/test262.git legacy-test262
git -C legacy-test262 checkout e1166500a0d26c8b90e704a31c6646992573f632
cargo build --release --locked -p rust-browser-js-harness
python3 tools/test262/legacy-vm.py --website legacy-test262/website --worker target/release/rb-js --output legacy-vm.jsonl
```

The runner verifies the Git blob hashes of the catalogs and helper scripts and checks that the 16,436 paths are unique. Use `--filter Array/prototype/indexOf` to run a subset. Each JSON row keeps its original 1-based Run All index. Windows uses `target/release/rb-js.exe`.

Add `--require-all-pass` to return exit code 1 when a selected case fails. Exit code 2 indicates a worker or input problem, including an external timeout. The default reports all results and returns 0 when the run completes without worker errors. Rows with `verdict_source: "website-callback"` received a result from the original validator. Rows marked `"vm"` identify a resource stop, unsupported feature, or missing completion. The VM runs promise jobs. Browser timers, rendering, and frame teardown require the native probe. For example, `built-ins/Object/defineProperty/S15.2.3.6_A1.js` also exercises a form element when `document.createElement` exists. Only a browser run exercises that branch.

The same fixture can test different language rules in different revisions. Case 1,680 of the legacy site replaces an iterator’s `next` method inside a `for…of` loop and expects the replacement to terminate iteration. The modern test at the same path throws if the loop reads `next` again and checks that it read the method exactly once. [2016 fixture](https://github.com/v8/test262/blob/e1166500a0d26c8b90e704a31c6646992573f632/test/language/statements/for-of/iterator-next-reference.js) · [Modern pinned fixture](https://github.com/tc39/test262/blob/48221d7c1f7745dc13e420a2e850a248c0b2afb4/test/language/statements/for-of/iterator-next-reference.js)

### Isolated VM resource diagnostics

The legacy worker normally uses its standalone VM limits, including 1,000,000 instructions per script. A separate diagnostic can apply Satin alpha.37 document VM limits and a chosen fuel budget to a selected set of original cases.

```sh
python3 tools/test262/legacy-vm.py --website /path/to/website --worker target/release/rb-js \
  --output unicode-diagnostic.jsonl --filter S7.4_A5.js --diagnostic-profile web-vm-alpha37 \
  --max-instructions 100000000 --timeout 30
```

The diagnostic accepts 1 through 100,000,000 instructions per script and defaults to 1,000,000. Each result records the requested profile, fuel, corpus revision, worker SHA-256 and process timeout. Completed worker responses also include the VM settings actually applied. Compare these isolated VM results separately from default VM reports and from the browser’s Run All counters. The browser additionally schedules callbacks, enforces timer deadlines, and shares resources across nested frame scripts.
