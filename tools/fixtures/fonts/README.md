# Font loading verification

Run the gate against a built browser. The output directory receives the process
receipt, server request log, final DOM, and a PNG from the browser renderer.

```powershell
node tools/verify-fonts.mjs target/debug/rust-browser.exe target/font-verification
```

To compare the same fixture in an isolated headless Chrome, pass the path to
the installed REA CLI script. `REA_CLI` can supply that path instead. Use
`--chrome` for a Chrome executable outside the default Windows installation.

```powershell
node tools/verify-fonts.mjs target/debug/rust-browser.exe target/font-verification --chrome-reference --rea C:/path/to/rea.mjs
```

Two localhost origins serve the document and its assets. The stylesheet
redirect changes the base for its relative font URLs. Two family aliases share
one ordered source list. The first source is a malformed WOFF2 and the second
is held until the initial CSSOM measurement. A third family receives valid font
bytes without a CORS grant, so it keeps the synthetic fallback.

The expected final widths are 32 px for each alias and 20 px for the denied
family and its fallback control. The PNG check verifies that the wider face
also paints ink beyond the fallback outline. The source fonts and their
generator are under `crates/engine/tests/fixtures/fonts`.
