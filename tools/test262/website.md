# Running the legacy Test262 website

Satin can run a chapter or the entire suite through the controls on the [original Test262 website](https://v8.github.io/test262/website/default.html). This is the legacy browser suite. The [modern Test262 runner](README.md) uses the pinned upstream JavaScript tests and a standalone Satin VM.

## Run a chapter in Satin

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
| Complete Run All | `--until complete --timeout-ms 1800000` |
| Observe Run All making progress, pause and open Results | `--until first` |

`--chapter` defaults to completion. A native run without a chapter defaults to Run All. Use `--until first` for a short progress check. The default observation timeout is 420,000 ms and the maximum is 1,800,000 ms. Startup, catalog and row availability, execution and Results observation each have their own deadline. Each invocation creates a fresh browser profile and writes a report, action file, screenshot and checkpoint evidence under `target/test262/website/run-*`. Use `--output DIR` to change the parent directory.

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
