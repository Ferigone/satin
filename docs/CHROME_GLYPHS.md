# Satin chrome glyphs

[Before/after raster sheet](chrome-glyph-comparison.png) · base `cd96dd68eaa77a4547b711cb5ddac0389c8f3082`

## Review

`crates/browser/src/app.rs` places toolbar and caption marks in 20-DIP tiles. The
old `Canvas::stroke_line` stamps filled squares; rounding each stamp outwards
makes a nominal 1.3-DIP stroke change weight with pixel phase and exaggerates
diagonals. The new navigation masks sample a rounded 1.5-DIP stroke and blend the
combined coverage once per device pixel.

| Glyph | Optical correction |
| --- | --- |
| Back / Forward | Exact mirrored geometry, centered shaft, shorter chevrons and even caps. The existing muted history color still denotes unavailable traversal. |
| Reload | The arrow meets the arc directly, with a visible gap to the tail. The loading/Stop mark does not change. |
| Address search | A smaller, inset lens and a joined 45° handle keep the mark away from the address border and aligned with its neighbors. |

Caption controls were reviewed separately. The short minimize bar, single
maximize frame, overlapping restore frames and close cross keep familiar desktop
silhouettes. Their stamped strokes are heavier, and the two restore frames crowd
each other at 100%. They remain unchanged; revisit them as a family with native
window-state and DPI checks rather than treating them as toolbar marks.

## Evidence and checks

The sheet uses the original and revised `chrome_icon` functions and the
identity-transform `Canvas` fill/blend path, with the app's dark surfaces and
foreground colors. It shows 100%, 125%, 150% and 200% at 2× nearest-neighbor, plus
native pixels at 100% and 200%. The toolbar's actual glyph origins and caption
origins at a nominal 1280-DIP window width preserve fractional pixel placement.
Sampled caption marks and all other sampled legacy glyphs are pixel-identical.
This is an isolated Windows x64 raster preview, not a captured live window.

As a weight check, the disabled Back shaft at local x=12 DIP yields the following
vertical contrast profile against `#1a1a1c`. Values are sRGB relative luminance
ratios and exclude background-only pixels. The unchanged disabled source color
`#8d898b` measures 5.03:1 at full coverage; partial edge pixels are intentionally
dimmer. These spot checks describe the raster, not an accessibility audit.

| DPI scale | Before (top to bottom) | After (top to bottom) |
| --- | --- | --- |
| 100% | 5.03, 5.03 | 3.40, 3.40 |
| 125% | 5.03, 5.03, 5.03 | 2.24, 5.03, 2.24 |
| 150% | 5.03, 5.03 | 1.45, 5.03, 5.03, 1.45 |
| 200% | 5.03, 5.03, 5.03, 5.03 | 2.24, 5.03, 5.03, 2.24 |

### For the next glyph pass

1. Keep the control bounds, input and state colors in `app.rs`. Design navigation
   marks on the existing 20-DIP grid; start with a 1.5-DIP stroke and about 2 DIP
   of clear space. Align ink by eye with neighboring marks, not just by matching
   the drawing bounds. Mirror directional pairs from one shape.
2. Check the actual raster at 100%, 125%, 150% and 200%, including fractional
   origins and unavailable states on the real chrome background. Review it at
   1:1 first. Inspect the stroke core and joins, plus small negative spaces such
   as the reload gap. For enabled marks use 3:1 against adjacent colors as a
   minimum design check, not an edge-pixel threshold.
3. Run the mask tests in `app/icons.rs` (mirroring, DPI-normalized coverage,
   connected cores and open gaps). From the repository root on Windows, these
   can run without building Satin:

   ```powershell
   $scratch = Join-Path (Get-Location) 'target\chrome-glyph-tests'
   New-Item -ItemType Directory -Force -Path $scratch | Out-Null
   $env:TEMP = $scratch
   $env:TMP = $scratch
   @'
   #[path = "../../crates/browser/src/app/icons.rs"]
   mod icons;
   '@ | Set-Content -LiteralPath (Join-Path $scratch 'tests.rs') -Encoding ascii
   $rustc = Join-Path $env:LOCALAPPDATA 'RustBrowserDev\rustup\toolchains\1.99.1-x86_64-pc-windows-msvc\bin\rustc.exe'
   & $rustc --edition 2024 --test (Join-Path $scratch 'tests.rs') -o (Join-Path $scratch 'tests.exe')
   if ($LASTEXITCODE -ne 0) { throw 'Glyph test build failed.' }
   & (Join-Path $scratch 'tests.exe')
   ```

4. For live verification with an already built browser, prefer `--smoke-window`
   over `--headless` so the PNG includes the chrome. Use a fresh `--profile`
   path per run and the display's native scale. Check restored/maximized and
   hover/pressed states separately on a Windows session reserved for UI testing.

The geometry and raster sheet are original project work under Satin's
MIT OR Apache-2.0 license. They do not extract glyphs from a system font or copy
another browser's assets. The official icon and start-page reference are not
part of this set.
