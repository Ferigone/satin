# Desktop downloads

On Windows, following an HTTP(S) attachment or clicking a same-origin link with the HTML `download` attribute opens **Save download**. The suggested filename comes from the response, the link or the final URL. Select a new destination to begin. Satin displays the filename and bytes written at the bottom of the tab. **Esc** cancels an active transfer. Closing its tab or the browser also cancels it. On Linux the desktop build uses the configured `XDG_DOWNLOAD_DIR` or `~/Downloads` and adds a number when a name already exists.

| Where the response goes | Decoded body limit | How Satin handles it |
| --- | ---: | --- |
| Fetch or XMLHttpRequest | 64 MiB (67,108,864 bytes) | Counts the decompressed bytes as they arrive. Exceeding the limit fails the response, including for gzip or a body without a declared length. |
| JSON source opened in a tab | 64 MiB | Streams JSON text into a source buffer and paints the visible rows using a sparse index. Supports `application/json` and JSON media types with a `+json` suffix. |
| File download | No total byte limit | Writes the original HTTP response directly to a temporary file beside the chosen destination. Each filesystem write is at most 64 KiB. Publishes the completed file without overwriting an existing file. |

A download can exceed 64 MiB even when the file is JSON. The transfer needs sufficient disk space and a working connection. Cancelling or receiving a truncated response discards the partial file. The Windows build sets the Internet zone on the completed download before making it available.

## Earlier design and audit

The following audit describes commit `cd96dd68eaa77a4547b711cb5ddac0389c8f3082`. It records the starting behavior, the offline row model, and proposals for a future downloads panel.

| Surface | Current behavior |
| --- | --- |
| Link activation | [`App::content_click`](../crates/browser/src/app.rs#L2295) uses [`ScriptRuntime::click_with_default`](../crates/engine/src/script.rs#L977) to honor click handlers and resolve the final `href`, then creates a navigation request. There is no download decision for `<a download>` or desktop “Save link as” action here. |
| Top-level HTTP responses | [`Loader::stream_controlled`](../crates/engine/src/loader.rs#L520) branches on MIME, not `Content-Disposition`, and hands the app [`NavigationPart`](../crates/engine/src/loader.rs#L327) (`Open`, decoded `Text`, `Finish`). HTML is fed to the document; the non-HTML branch reads a bounded body and [builds a document](../crates/engine/src/loader.rs#L445), not a saved file. There is no attachment handoff before document commit. |
| Stop, Fetch, and media | [`Action::Reload`](../crates/browser/src/app.rs#L864) stops tab loading without closing the document. [`jobs.rs`](../crates/browser/src/jobs.rs#L23) queues navigation, script Fetch and document resource work by tab/navigation, not desktop downloads. Their bodies, progress and cancellation belong to those consumers. |
| Native file picker | [`file_picker.rs`](../crates/browser/src/file_picker.rs#L189) uses `GetOpenFileNameW` for web file inputs and reads selected files with limits. It is not a save chooser or authorization to write to the selected paths. |
| Installed updater | [`updater.rs`](../crates/browser/src/updater.rs#L195) stages a Satin release under the installer's `versions` directory, verifies size, SHA-256 and the x64 executable, and publishes an install manifest. It is not a generic saver, download source, or panel item. |

There is no desktop download job, authorized destination, file publication/cleanup service, or safe open/reveal adapter behind a panel. A second HTTP client or a refetch from app chrome would not preserve the original response or its navigation policy. Do not route attachments through the updater, script Fetch, or the document display path as a substitute.

## Recommended Windows UX

[Chrome's desktop tray design](https://blog.chromium.org/2023/08/redesigning-chrome-downloads-to-keep.html) puts recent activity beside the address bar rather than in a persistent shelf. It exposes inline cancel and folder actions. [Chrome Help](https://support.google.com/chrome/answer/95759) describes opening a completed file and removing history without deleting the file. [Firefox's current desktop strings](https://searchfox.org/mozilla-central/source/browser/locales/en-US/browser/downloads.ftl) distinguish completed, failed, canceled and blocked states and use “Show in Folder” on Windows. For Satin:

- Once real jobs exist, start with a compact native panel opened by **Downloads** in the existing menu or **Ctrl+J**. No permanent toolbar button or change to tab/address dimensions is needed. Show newest first and keep rows keyed by stable, session-unique IDs. Include a “No downloads this session” empty state only once the feature is live. Keep the list session-scoped initially; defer a full history view.
- Show a basename and status in text, not just a color or icon. For known sizes, show bytes and a determinate bar; for unknown sizes, show bytes written with no percentage or made-up ETA. Reaching the expected count means “Finishing…”, not “Ready”. Only a confirmed save makes a file actionable.
- “Cancel” affects one row, even after switching tabs. Show “Canceling…” until the service confirms the outcome. “Show in folder” selects the saved file in Explorer; it does not open it. Prefer reveal as the first production capability. Offer an explicit “Open” button only when local policy and a safe handler permit it; no automatic open, no click-to-open on a file name. If the file moved, vanished or was replaced, show “File unavailable” and remove its actions.
- Use visible focus, keyboard traversal and Enter/Space on focused actions; Escape dismisses the panel and restores focus. Do not pass panel clicks or keys through to the page. Give actions and progress accessible names and announce status changes without announcing every byte update. Keep the full checked basename accessible if the renderer elides it; preserve the extension in narrow layouts. When an action disappears, move focus to its row or another enabled control, never to an Open button by default.
- Do not include Retry, Pause/Resume, Clear, Delete, or auto-open settings in the first slice. Any future “Remove from history” action must not delete file contents. Do not steal focus when a download finishes.

### Offline model

[`app/downloads_ui`](../crates/browser/src/app/downloads_ui/mod.rs) turns typed service events into row text, progress and available actions. It neither performs I/O nor receives URLs or paths. A native user action yields an ID-only intent; `Cancel` also enters the pending state. The eventual controller must deliver cancellation or report a terminal error rather than leave a row pending forever. The worker arbitrates cancel versus publication and emits one terminal outcome. Late progress or duplicate terminal events cannot revive a canceled or failed row. A later verified file-unavailability event can invalidate a ready row.

[`tests/downloads_ui.rs`](../crates/browser/tests/downloads_ui.rs) is the only caller. The printed fixture is explicitly labeled offline; its sample names are not files. No production module registration, menu item, or demo flag is added.

## What must land before wiring the panel

1. **A real attachment handoff, outside this app-only change.** Start with a user-initiated, top-level HTTP(S) GET and an attachment response. Route the final response to an application download service before opening/decoding a document, using the original bounded stream, redirects and request context. Keep the current document and history intact; do not refetch. Do not turn a subresource, Fetch, script-triggered or sandboxed request into a save dialog. Defer `<a download>`, Blob/data URLs, resumption and automatic downloads until their policy semantics are covered. See [WHATWG downloading resources](https://html.spec.whatwg.org/multipage/links.html#downloading-resources) and [RFC 6266 §4.3](https://www.rfc-editor.org/rfc/rfc6266#section-4.3).
2. **Explicit save authorization.** Use a window-owned [`IFileSaveDialog`](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-ifilesavedialog) constrained to filesystem paths. No destination writes until the user accepts; dismissal drops the body without saving. A header `filename*` (prefer a valid one over `filename`) is only a suggestion. Remove directory components and reject or replace invalid basename suggestions: controls, bidi overrides, separators, colon/alternate streams, reserved device names (including with extensions), trailing dots/spaces, `.`/`..`, and overlong names. Do not trust the response MIME or suffix as proof that a file is harmless. Revalidate the chosen path too. For the first slice, accept only a new regular file in a user-selected local directory; no automatic fallback, overwrites, devices, UNC paths, or reparse-point traversal. See [Windows naming rules](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file).
3. **Bounded writes and race-safe publication.** Bind operations to the authorized directory; a one-time path check does not protect against a junction or symlink swap. Use an exclusively created, inert, uniquely named partial file there (or protected disposable staging), cap bytes/jobs/deadlines independent of advertised length, and count bytes written rather than bytes read. Close and validate before publishing into the selected directory with no-replace semantics. Resolve cancellation and publication so only one wins. Clean up only the owned partial file on cancel/failure; report a cleanup failure separately. Do not expose a partial, create a final file before approval, or claim completion before publication and attachment policy succeed.
4. **Windows protection and file identity.** Evaluate [`IAttachmentExecute::Save`](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-iattachmentexecute-save) with the actual source and candidate file. It may scan, alter or delete content, and may write origin evidence in an NTFS alternate data stream. Verify policy for the final name/type, preservation of required origin marking through publication, and behavior on volumes without streams. If checks fail or evidence cannot be preserved, do not quietly mark the file ready. Never call an attachment execution API on completion. For a user-pressed action, resolve the ID to the committed file in the controller, check policy and revalidate its identity, not just path existence. Use [`SHOpenFolderAndSelectItems`](https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shopenfolderandselectitems) from a COM-initialized thread for reveal; do not build command lines from names or URLs. Keep Open unavailable for executables, scripts, shortcuts and unknown or policy-blocked types. Keep it unavailable for all files if the risk of replacement at shell handoff has not been addressed.

Once those pieces are testable, wire a native overlay and session list to the app event loop. The shell adapter must still enforce policy at activation time; a row's cached presentation state is not authorization.

## Verification

Run the dependency-free fixture from the repository root with the pinned Rust toolchain, without rebuilding the browser:

```powershell
$rustc = 'rustc'
$tools = Join-Path $env:LOCALAPPDATA 'RustBrowserDev'
if (Test-Path (Join-Path $tools 'cargo\bin\rustc.exe')) {
    $env:CARGO_HOME = Join-Path $tools 'cargo'
    $env:RUSTUP_HOME = Join-Path $tools 'rustup'
    $rustc = Join-Path $env:CARGO_HOME 'bin\rustc.exe'
}
New-Item -ItemType Directory -Force target/downloads-ui | Out-Null
& $rustc +1.99.1 --edition=2024 --test -D warnings `
    crates/browser/tests/downloads_ui.rs -o target/downloads-ui/downloads_ui.exe
if ($LASTEXITCODE -ne 0) { throw 'Downloads UI fixture failed to compile.' }
& .\target\downloads-ui\downloads_ui.exe --show-output
if ($LASTEXITCODE -ne 0) { throw 'Downloads UI fixture failed.' }
```

The checks assert four primary row presentations (including their action labels), monotonic byte counts and indeterminate progress, no file actions at 100% before save acknowledgment, cancel acknowledgment and both sides of the cancel/commit race, wrong-ID and late-event rejection, reveal-only and lost-file actions, and safe failure messages. They do not launch a browser or Explorer and do not create any download files.

Future release gates, using only generated bytes on loopback, a new disposable destination, and mocked launch adapters unless explicitly testing Explorer:

| Test | Expected result |
| --- | --- |
| Serve one `attachment` response containing `b"Satin offline fixture\n"` with a length, and one without; count requests. Accept Save As. Repeat with a plain inline page and a renderer Fetch to the attachment URL. | One request per accepted link. Previous page, URL and history remain intact. Destination bytes match exactly; no partial remains. Progress has a percentage only when the size is known. Ready. Inline navigation and renderer Fetch do not create jobs or show a save prompt. |
| Dismiss Save As; on a separate run, send one chunk, pause the server using a test barrier, switch or close the source tab, then cancel from the panel. | No destination file from dismissal. The panel cancel stops the transfer by download ID, cleans the partial and produces Canceled only after worker acknowledgment. Tab Stop/Reload does not cancel the accepted download. |
| Advertise a length, send a short prefix, then close the socket. Inject a write/publish error and a cleanup error in separate runs. | Failed with the matching reason, no Open/reveal. No final file; ordinary failures remove the owned partial. Cleanup failure is reported without claiming deletion. |
| Use fixture suggestions such as `../notes.txt`, `C:\outside.txt`, `NUL.txt`, `COM¹.txt`, `note.txt:stream`, a trailing dot, and a bidi override. Introduce an existing target or swap a junction at publication. | Safe basename suggestion or rejection; only the chosen local directory can be written. Existing sentinel bytes are unchanged and no file is created through the junction. |
| Exercise attachment check denial and missing required provenance. After one authorized benign save, move, delete or replace the file. Use a mocked handler for any proposed Open capability. | Denied/unmarked files never become Ready. Unavailable files lose both actions; stale input makes no shell call. For a still-valid file, manual “Show in folder” selects it without opening it. Open remains disabled for disallowed types; the mock sees exactly one launch on explicit Open and none on completion or reveal. |
| Compare the normal home screenshot before and after wiring; use pointer and keyboard on the panel at 100% and 150% DPI. | Home and closed chrome are unchanged. When open, the panel has readable statuses, reachable actions and visible focus, and Escape restores focus without activating the page. |
