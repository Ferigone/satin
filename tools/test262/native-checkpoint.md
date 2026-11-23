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

After a coordinator-owned **public harness** attempt (not the two-case fixture), save the exit record and a manifest beside the existing stdout log and final frame. Set the suite version/date to the intended known reference, not values guessed from missing snapshots.

```json
{
  "schema": 1,
  "scope": "legacy-test262-native-chapter",
  "url": "https://v8.github.io/test262/website/default.html",
  "selection": { "kind": "chapter", "name": "harness", "planned": 55 },
  "suite": { "version": "1234", "date": "2016-03-23" },
  "files": { "process": "process.json", "stdout": "native.out", "frame": "frame.png" }
}
```

```powershell
@{ code = $r.ExitCode } | ConvertTo-Json | Set-Content (Join-Path $out 'process.json') -Encoding UTF8
Copy-Item -LiteralPath $r.StdoutPath -Destination (Join-Path $out 'native.out')
node tools/test262/native-checkpoint-verify.mjs (Join-Path $out 'manifest.json')
```

Keep the exit record and both `.run.json` / `.results.json` sidecars in that same fresh directory. A timeout or launch exception has no successful exit record. If the owning runner recorded a timeout, set `timedOut: true` rather than inventing an exit code. The tool does not launch a browser. `artifacts-consistent` means the receipt, PNG container, scope, selected panes and counters agree; it is not a visual check or a published score. Confirm that the actual `harness` Run image was used and that the final PNG displays the matching Results. Page diagnostic strings are kept as metadata, not used as pass/fail criteria.

`observed` shows raw DOM observations on failed and partial attempts too: `"0"` is an observed zero, `""` is blank, and `missing-element`, `missing-snapshot`, and `invalid-snapshot` are distinct. These fields are not proof that a pane was visible or that its counters belong to a completed run.

### Coordinator website report intake

Pass the same manifest to the website probe once the capture exists:

```powershell
node tools/test262/website.mjs --native-chapter-manifest (Join-Path $out 'manifest.json') --output target/test262/native-website
```

This mode only reads existing evidence and writes a report. It cannot be combined with `--satin`, `--native`, `--chapter`, or Chromium capture flags. It will not mark a run complete from JSON snapshots and a PNG alone.

For completion, add `files.actions` (the actual smoke action file) and `files.selectionFrame` (the frame used to map the chapter Run hit), also beside the manifest. The actions must be the exact sequence: optional expected title, Run tab click, one chapter `click_at`, Run completion checkpoint, Results tab click, Results checkpoint. Use the checkpoints in the coordinator recipe with up to 15,000 ms each. A Run All, Select, or extra input is not this proof.

Add `visualReview` only after a reviewer can unambiguously confirm the actual replay point maps to the **Run image on `Chapter - harness (55 tests)`**, and that **current Results** and all four Results counters are visible in the final frame. A preflight screenshot by itself is not proof the point was correct in this capture. If the layout, scale, actual hit, or current Results cannot be confirmed, leave the review absent. The tool checks the attestation and byte bindings, not the image pixels or reviewer identity.

```json
{
  "reviewer": "",
  "runId": "",
  "sha256": { "actions": "", "selectionPng": "", "png": "", "run": "", "results": "" },
  "selection": { "label": "", "control": "", "x": null, "y": null, "scale": null },
  "results": { "current": false, "counters": { "ran": null, "pass": null, "fail": null, "failedToLoad": null } }
}
```

Set `runId` from native stdout and the five hashes from the raw artifact files (`actions.json`, selection frame, final frame, Run sidecar, Results sidecar). `selection.x/y` are the actual logical `click_at` coordinates; `scale` is the verified window scale. Copy Results counters as observed integers; a missing field is not zero. An unreviewed, partial, timed-out, or changed-document capture stays `websiteRun: "unverified"` with no completion counters. An imported completion does not imply a separately observed first count.

Offline verifier tests: `node --test tools/test262/native-checkpoint-verify.test.mjs`.
