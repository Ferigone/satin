# Software renderer microbenchmark

`tools/fixtures/render-cost/gallery.html` describes a local, fixed 960 × 640
gallery of overlapping, fractionally positioned raster images and a few labels.
The `render_cost` example supplies synthetic RGBA pixels and the existing
synthetic `wide.ttf` test font. It runs offline, uses no browser profile, and
calls the same `Canvas::document` used by the browser's headless screenshot
path. Use the example, not the HTML by itself: there are no PNG files to fetch.

## Run it

From the checkout root, in PowerShell with the private pinned toolchain:

```powershell
$private = Join-Path $env:LOCALAPPDATA 'RustBrowserDev'
$env:CARGO_HOME = Join-Path $private 'cargo'
$env:RUSTUP_HOME = Join-Path $private 'rustup'
$env:CARGO_TARGET_DIR = Join-Path (Get-Location).Path 'target\render-cost'
$env:CARGO_PROFILE_RELEASE_LTO = 'false'
$env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS = '8'
$cargo = Join-Path $private 'cargo\bin\cargo.exe'
$mutex = [Threading.Mutex]::new($false, 'Local\SatinA18CargoHeavy')
$held = $false
try {
    $held = $mutex.WaitOne([TimeSpan]::FromMinutes(15))
    if (!$held) { throw 'Timed out waiting for the shared build slot' }
    & $cargo +1.99.1 build --offline --locked --jobs 1 --release -p rust-browser-engine --example render_cost
    if ($LASTEXITCODE -ne 0) { throw "cargo exited $LASTEXITCODE" }
} finally {
    if ($held) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
$bin = Join-Path $env:CARGO_TARGET_DIR 'release\examples\render_cost.exe'
1..3 | ForEach-Object { & $bin }
```

For a before/after comparison, keep the same fixture, compiler and settings.
Copy the baseline executable before rebuilding, then alternate fresh baseline
and candidate runs on the same machine. Compare `pixel_hash` and all
non-timing counters as well as timings. Samples are not a CI speed gate.

## What the counters mean

- `cold_paint_ns` is one first paint with an empty glyph bitmap cache.
  `warm_paint_p50_ns` and `warm_paint_p95_ns` describe the next 31 paints on the
  same layout and renderer. Each frame starts cleared. The timer covers only
  `Canvas::document`, not the clear, layout, asset generation or any process
  startup, decoding, PNG encoding, OS presentation or GPU work. Cold here
  refers to the raster cache, not the operating system or CPU caches.
- `image_pixel_samples` is the sum of viewport-clipped axis-aligned image
  footprints using the painter's integer bounds. The excess over
  `unique_image_destination_pixels` counts repeated destination samples,
  including alpha-zero samples. It is not total overdraw for all paint
  commands, actual writes avoided, or visible coverage after compositing.
- `decoded_image_payload_bytes` sums the lengths of four RGBA buffers once
  each, including the offscreen-only asset. Eight image commands share these
  four assets. Weak references verify that dropping the page leaves assets
  held by the layout and dropping that layout releases the last strong owners.
- `glyph_bitmap_bytes` lists the bitmap payload lengths before paint, after
  first paint, after warm paint, and after up to one prune interval with the
  gallery gone. The example also checks that the glyph cache did not keep the
  synthetic font alive. These byte counts exclude buffer capacity, allocator
  overhead, cache metadata and other allocations; neither is an RSS metric.
