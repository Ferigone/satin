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

The desktop checks exercise local navigation, script events, forms, and real MP4 playback. The native media check clicks Play in `about:media` and waits for the video clock to advance. The verifier writes results and screenshots under `target`.

## Website compatibility

| Feature | Current alpha |
| --- | --- |
| Progressive MP4 | H.264 video and AAC audio on Windows, with play, pause, and seek |
| YouTube | The page reaches its initial video metadata. Player initialization remains in development. |
| Twitch | The page loads its HTML. Its startup scripts reach parser and runtime limits before the video player appears. |

The selected web-platform and JavaScript test suites measure the behavior their cases cover. [Roadmap](ROADMAP.md) · [Test262](../tools/test262/README.md)
