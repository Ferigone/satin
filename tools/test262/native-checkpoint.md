# Native smoke checkpoint

This opt-in action runs only under the existing `--smoke-window` / `--smoke-actions` path. It reads the active native tab after the preceding `click` or `click_at`; it does not run JS, modify the page, or launch a second process. Example actions for the owned `fixtures/native-checkpoint.html`:

```json
[
  {"action":"click","id":"start"},
  {"action":"checkpoint","name":"run","within_id":"run-state","descendant_class":"status","text":"Done","timeout_ms":1000},
  {"action":"click","id":"results"},
  {"action":"checkpoint","name":"results","within_id":"report","descendant_class":"ran","text":"2","timeout_ms":1000}
]
```

The condition must be unmet immediately before input and met within its deadline. The action requires a unique connected root and exact-text/class descendant. It checks a native layout hit within the viewport, rejecting missing, `hidden`, display-none, offscreen, or obscured hits in Satin's current layout. This is not a pixel or handler oracle. All checkpoints share one tab and document navigation generation. An ordinary fragment change is allowed; replacing the document is not.

For `--smoke-window frame.png`, it writes `frame.png.run.json` and `frame.png.results.json` with `runId`, `tabId`, `documentNavigation`, `url`, `title`, and serialized `html`. Names cannot contain paths. A checkpoint never overwrites a file. The last condition and document must still match when the final PNG is captured.

The successful stdout report includes `checkpoints: { runId, names }`, with the same attempt ID as both sidecars and names in action order. A normal failure rolls back only sidecars created by this attempt; a hard kill or cleanup error may leave files but cannot issue that success report. Use a fresh, exclusively owned output directory and require zero exit, matching attempt IDs and ordered checkpoint names, valid sidecars, and the final PNG; verify expected states and counters externally. IDs identify attempts but are not an attestation of untouched artifacts. An observed transition is not by itself proof of which pointer recipient or callback caused it.

## Coordinator validation

Run the focused unit tests once in the authorized build worktree:

```powershell
cargo +1.99.1 test --locked --offline -j 1 -p rust-browser --bin rust-browser testing::tests::
```

Then use a digest-checked executable containing the patch. From the repository root, with the JSON above saved as `actions.json` in a new `$out` directory:

```powershell
. .\scripts\Process.ps1
$fixture = (Resolve-Path .\tools\test262\fixtures\native-checkpoint.html).Path
$r = Invoke-BrowserProcess -Executable $exe -TimeoutMilliseconds 15000 -LogDirectory (Join-Path $out 'logs') `
    -Arguments @('--url', ([Uri]::new($fixture).AbsoluteUri), '--profile', (Join-Path $out 'profile'),
      '--smoke-window', (Join-Path $out 'frame.png'), '--smoke-actions', (Join-Path $out 'actions.json'))
$r | Select-Object ExitCode, Stdout, Stderr
```

Use fresh directories for all controls. The positive fixture must return zero, create both sidecars with one attempt and document identity, and show Results in the final PNG. The Run snapshot must say `Done` while the report is still hidden; the Results snapshot must show the same count, `2`, after reveal. These are two local observations, not a public test score. For negative controls:

| Change to fixture actions or output | Required outcome |
|---|---|
| Replace `start` with `wrong` | Timeout at `run`, no `run` sidecar. |
| Replace `results` with `wrong` | Results remain hidden and `results` times out; the earlier `run` sidecar is rolled back. |
| Replace `start` with `duplicate` | Fail on ambiguous matching descendants, even though one is hidden. |
| Replace `results` with `reload` | Fail on changed document identity, even though the URL is the same. |
| After `run`, insert a `wrong` click and a new checkpoint for `run-state .status` / `Done` | Fail before input because the condition already matched. |
| Replace `results` with `rollback`, then append `{"action":"wait_title","value":"Fixture rolled back","timeout_ms":1500}` | Fail because the last condition no longer matches at the final frame; roll back both sidecars. |
| Pre-create `frame.png.run.json` with sentinel content | Fail and leave that file unchanged. |

All negative cases must return nonzero and no final success summary. In the normal failure controls there should be no sidecars newly created by this attempt. Regression-check one existing smoke action sequence without checkpoints (stdout has no `checkpoints` field). Only the coordinator should try the public website afterward. For that run, use the real `harness` row and exactly two checkpoints (`#progressbar .text` / `Testing complete!`, then `#contentContainer .totalCases` / `55` after clicking Results); no Run All, script changes, or inferred first-count sample. The score additionally requires reconciled Run counts, matching same-session Results, and visual evidence—not merely the presence of sidecars or any expected diagnostic text.

## Read-only harness artifact check

After a coordinator-owned **public harness** attempt (not the two-case fixture), save the exit record and pass it alongside the existing stdout log and final frame:

```powershell
@{ code = $r.ExitCode } | ConvertTo-Json | Set-Content (Join-Path $out 'process.json') -Encoding UTF8
node tools/test262/native-checkpoint-verify.mjs (Join-Path $out 'process.json') $r.StdoutPath (Join-Path $out 'frame.png')
```

Keep the exit record and both `.run.json` / `.results.json` sidecars from that same fresh directory. A timeout or launch exception has no successful exit record. The tool does not launch a browser. `artifacts-consistent` means the receipt, PNG container, scope, selected panes and counters agree; it is not a visual check or a published score. Confirm that the actual `harness` Run image was used and that the final PNG displays the matching Results. Page diagnostic strings are kept as metadata, not used as pass/fail criteria.

Offline verifier tests: `node --test tools/test262/native-checkpoint-verify.test.mjs`.
