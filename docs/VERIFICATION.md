# Verification

Run the Rust suite on Windows x64.

```powershell
cargo test --workspace --locked
```

The suite covers the JavaScript VM, DOM, layout, Web Platform Tests fixtures, and Windows-specific behavior. [JavaScript](JAVASCRIPT.md) · [CSS layout](CSS_LAYOUT.md)

## Desktop smoke tests

From the repository root, build and verify the Windows release.

```powershell
.\scripts\Build.ps1 -Release
.\scripts\Verify.ps1
```

The desktop checks exercise local navigation, script events, forms, and real MP4 playback. They navigate to a relative HTML path with an encoded space using `location.assign()` in headless mode and a regular link in a native window. Both verify that the destination runs its own script. The verifier also checks `about:media` and `about:streaming` in both modes. The streaming example advances its video clock before appending the final audio and video fragments. It uses a generated motion pattern and a sine tone. Results and screenshots go under `target`.

The headless and desktop smoke reports include playback time, readiness, video dimensions, decoded frame count, and audio-track detection for each active media element.

The verifier also clicks the test button on `about:workers` in both modes. The page transfers a typed-array backing buffer to a classic Blob worker and checks that ownership moved. The worker changes one byte, executes an authored WebAssembly module, and sends the modified view and numeric result back to the page.

To offer the exact verified build through Satin Preview, pin its executable digest instead of rebuilding on click:

```powershell
$digest = (Get-FileHash .\target\release\rust-browser.exe -Algorithm SHA256).Hash
.\scripts\Preview.ps1 -NoLaunch -VerifiedReleaseSha256 $digest
.\scripts\Preview.ps1 -CreateShortcut -VerifiedReleaseSha256 $digest
```

The pinned shortcut can use the matching snapshot after a subsequent build and still uses the separate Satin Preview profile.

## Website compatibility

| Feature | Current alpha |
| --- | --- |
| Progressive MP4 | H.264 video and AAC audio on Windows, with play, pause, and seek |
| Segmented MP4 | Separate H.264 and AAC buffers, with frames and time advancing before the last append |
| Worker and WebAssembly | Classic Blob workers exchange messages and transferable ArrayBuffers. WebAssembly runs a bounded subset of integer instructions and calls imported JavaScript functions. |
| YouTube | More of the page bootstrap runs, including a large configuration script. Player initialization remains in development. |
| Twitch | User Timing and additional JavaScript packages run during startup. The remaining script limits still prevent the player from appearing. |

On Windows, a seek near the tail just before a stream finishes appending can occasionally stall resumption. The native checks also cover seeking after the stream is finalized.

The selected web-platform and JavaScript test suites measure the behavior their cases cover. [Roadmap](ROADMAP.md) · [Test262](../tools/test262/README.md)
