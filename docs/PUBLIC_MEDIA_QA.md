# Public media compatibility QA

This is an anonymous smoke test of the live web, not a deterministic playback test. The reference is a public Twitch channel (`theburntpeanut_247`) and the public YouTube upload `aqz-KE-bpKQ`. An offline channel, pre-roll, consent screen, sign-in wall, or transient connection failure is a separate outcome, not evidence of a browser regression. Run the reference, installed baseline, and candidate as close together as possible.

Run from the worktree root with an already installed [REA](https://github.com/morluto/rea) CLI and Google Chrome. Pass the exact browser executable, not the Satin launcher. Do not use an existing browser profile or run the installer.

```powershell
$rea = Join-Path $env:TEMP 'rb-youtube-qa-20261123\rea\node_modules\rea-agents\scripts\rea.mjs'
$chrome = 'C:\Program Files\Google\Chrome\Application\chrome.exe'
node tools/verify-public-media.mjs chrome $chrome $rea
node tools/verify-public-media.mjs satin $binary $sourceCommit
```

Append `twitch` or `youtube` to repeat just one site. See the [2026-11-23 comparison](PUBLIC_MEDIA_QA_2026-11-23.md) for an example of observed versus untested results.

The runner prints the path of a new sanitized `target/public-media/<run>/summary.json`. It records UTC timestamps, binary SHA-256, reported version, supplied source commit, OS, architecture, and viewport. The candidate is run read-only, with per-case profiles. The runner retains no raw DOM, cookies, storage, headers, network bodies, media URLs, or chat messages. The transient profiles and captures are removed on normal completion. If a run is interrupted, treat its `transient` directory as private; do not publish it.

## Reading the receipts

- **REA scenario:** Runs against an owned headless Chrome process, retains only a URL capture in memory, and reports whether the wait for a player or consent shell completed. This does not establish playback. A separate Playwright run using the library bundled with REA takes DOM/media measurements and exercises actual controls on a different fresh profile. Neither drives the shared desktop or signs in.
- **Scripts and diagnostics:** Satin reports its own loader's `executed_scripts` and first runtime diagnostics. Chrome's diagnostic count of executed scripts is the number of distinct HTTP(S)-named script IDs with positive precise-CDP coverage in the page target during the run; workers are not measured. Do not treat these counters as identical. Errors retained are sanitized classes and allowlisted causes, not original lines.
- **Playback:** Look for a real `<video>` inside the player, positive video dimensions, and increases in media time and video frames between timestamped samples in the same run. Check whether it is the requested content rather than an ad. A player container, poster, metadata, or one nonzero timestamp is not sufficient.
- **Controls:** A click alone does not pass. A play or resume click needs a subsequent advancing clock; pause needs a paused state and stable clock; seek needs a resulting clock jump. A live player without a seekable range and scrubber is **not tested** for seek. The Satin headless lane records one final snapshot only, not an in-session time series. If Satin creates a player, add a same-session native smoke trace before making a playback or controls claim.
- **Audio:** Browser-reported decoded audio bytes, audio tracks, or decoder are evidence about decoding or track presence only. They are not proof that sound was heard. Keep audible-output checks separate.
- **Chat:** Twitch read-only evidence is the count of rendered message elements where available, not message content. Do not type, send messages, or sign in. YouTube live chat is not applicable to this regular upload.

For replayable engine checks independent of a changing site, see [Verification](VERIFICATION.md).

## Localizing JS startup failures

Build an **uninstalled diagnostic binary** with
`cargo build --locked -p rust-browser --features media-startup-diagnostics`.
Use a distinct Cargo target and run it through the same Satin probe. The
optional feature adds bounded counters and Rust callsite tags for instruction,
native work, heap, and compiler limits; it does not raise any limits or change
what code the scripts are allowed to run. The sanitized receipt retains only
known record types and engine source paths. For an `instruction` record, the
location is the **last** native charge, not necessarily the main cost.
Instrumentation can affect timing; compare with an ordinary build.

`node tools/probe-media-capabilities.mjs $binary $chrome $rea $scratch` uses
one local document and reports advertised MIME/codec and player API support
side by side. It does not establish playback. Run `verify-native-media.mjs`
separately to exercise the native decoder with known content.
