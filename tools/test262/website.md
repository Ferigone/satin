# Public legacy Test262 website probe

`website.mjs` navigates to [the original public Test262 website](https://v8.github.io/test262/website/default.html) with fresh profiles. It runs the live site in each selected browser, without replacing assets, changing URLs, calling the site's controller from automation, or evaluating test source in Node. It does not run modern Test262 or produce a comparable score.

Use Node 22.15+ and existing browser executables. No packages, browser installation, or desktop interaction are required. From the repository root (PowerShell):

```powershell
node tools/test262/website.mjs `
  --satin 'target/release/rust-browser.exe' --native `
  --chromium 'C:/Program Files/Google/Chrome/Application/chrome.exe' `
  --chromium 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe' `
  --until first --output target/test262/website
```

Provide `--satin`, one or more `--chromium` paths, or both. Omit `--native` if hidden-window smoke mode is unavailable. `--satin-run-ms` changes the headless task budget (default 4,000 ms).

- **Satin:** captures a load snapshot and independent headless `#run` and `#btnRunAll` dispatches. A headless by-ID dispatch can reach hidden controls; the `run-all-by-id-diagnostic` artifact and `firstResultByIdObserved` never establish a website score. The optional hidden-window smoke mode also clicks Run, then attempts Run All in the same session through normal content hit testing. It can capture a frame or a failed click but cannot export a post-sequence DOM snapshot. `websiteRun` stays `unverified`, even if both clicks succeed.
- **Chrome/Edge:** uses the owned browser's local DevTools connection. It captures DOM state in an isolated world and sends pointer events only to a visible, hit-tested target. The default `--until first` clicks Run → Run All, observes a positive count, clicks Pause, checks Results, and resets. `first-result` is the **first observed** nonzero count, not a guarantee of a single executed case.
- **Full reference run:** `--until complete` leaves Run All uninterrupted and waits up to `--timeout-ms` (default 420,000 ms) for the page's completion marker. It requires reconciled counters, the planned number of executed tests, and matching Results. If it times out, `lastObserved` is the last responsive sample, **not** a final score. The full-run option does not change what the Satin CLI can verify.

Each invocation writes `run-*/report.json` and raw evidence under `--output`: executable digests, launch arguments, counters and stage observations, diagnostics, DOM, and Satin screenshots. Chromium also logs site requests/statuses, available response identifiers, and the browser build. The live endpoint can change between runs; keep the report with its UTC date and site version/date. Missing elements are `null`, blank counters are `""`, and an observed zero is `"0"`. “Failed to load” is included in “Fail,” not added to it. Expected iframe prerequisite exceptions and unrelated resource errors remain in the diagnostic log; they do not by themselves mean the workflow failed.

Exit `0` means all requested runs reached their reference target (`partial` for the first-result workflow or `complete`); exit `1` means at least one run is `unverified`, including a Satin-only capture; exit `2` means invalid invocation or inability to write a report. A nonzero exit does not turn blank or partial counters into a score.

The report interpretation tests require no browser or network:

```powershell
node --test tools/test262/website.test.mjs
```
