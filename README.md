# Satin

Satin is an open-source web browser with its own Rust implementations of HTML, CSS, DOM, layout, rendering, and JavaScript. Windows x64 is the first target. System libraries provide windowing, TLS, fonts, and other platform services.

The browser is in alpha. YouTube and Twitch are priority compatibility targets. The desktop build can open pages and run supported web APIs, while modern site initialization and adaptive playback still need work. Chrome is the reference for comparisons through REA.

## Build and run

Use Windows x64 with the repository's pinned Rust MSVC toolchain, Visual Studio C++ Build Tools, and the Windows SDK.

```powershell
.\scripts\Build.ps1 -Release -Test
.\scripts\Install.ps1 -SkipBuild
```

The installer creates a desktop shortcut and keeps a versioned copy of the browser. The installation and persistent profile are stored under `%LOCALAPPDATA%\RustBrowser`. The supplied rollback script activates the preceding build.

## Try the browser

- `about:home` opens the start page and the built-in examples.
- `about:version` lists features and current limitations.
- `about:javascript` exercises events, timers, and DOM changes.
- `about:graphics` exercises layout, SVG, and scrolling.
- `about:canvas` exercises Canvas 2D and pixel readback.
- `about:bytes` exercises file and streaming APIs.
- `about:messages` exercises transferable buffers and MessagePort.
- `about:files` opens an image selected through the Windows file picker.

Ctrl+L focuses the address bar, Ctrl+T opens a tab, Ctrl+W closes it, and Ctrl+R reloads. Use Ctrl+plus, Ctrl+minus, and Ctrl+0 for zoom.

## Project layout

- `crates/browser` contains the desktop UI, profile, and navigation driver.
- `crates/engine` contains networking, HTML, DOM, CSS, layout, and painting.
- `crates/javascript` contains the parser, compiler, VM, and built-ins.
- `crates/js-harness` runs JavaScript compatibility tests.
- `crates/launcher` starts the selected installed version.

Run `cargo test --workspace --locked` for the Rust suite. The `tools` directory contains REA scenarios and runners for pinned Web Platform Tests and Test262 cases. [Verification](docs/VERIFICATION.md) · [Roadmap](docs/ROADMAP.md) · [Releases](https://github.com/Ferigone/satin/releases)

## License

Satin is available under either the MIT license or Apache License 2.0. Third-party fixtures and datasets retain the licenses listed with their sources. [MIT](LICENSE-MIT) · [Apache 2.0](LICENSE-APACHE)
