param([switch]$Network)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Process.ps1')
$projectDirectory = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$binary = Join-Path $projectDirectory 'target\release\rust-browser.exe'
$qaDirectory = Join-Path $projectDirectory 'target\verification'
$logs = Join-Path $qaDirectory 'logs'
New-Item -ItemType Directory -Path $qaDirectory -Force | Out-Null
$version = Invoke-BrowserProcess -Executable $binary -Arguments @('--version') -LogDirectory $logs
if ($version.ExitCode -ne 0 -or $version.Stdout -cnotmatch '^Satin [0-9]') { throw 'The executable did not report a valid Satin version.' }
$render = Invoke-BrowserProcess -Executable $binary -Arguments @('--headless','--url','about:demo','--screenshot',(Join-Path $qaDirectory 'demo.png'),'--dump-dom',(Join-Path $qaDirectory 'demo.html'),'--height','1600') -LogDirectory $logs
if ($render.ExitCode -ne 0) { throw $render.Stderr }
$document = $render.Stdout | ConvertFrom-Json
if ($document.commands -lt 20 -or $document.links -lt 3) { throw 'The demo returned an incomplete render.' }
$window = Invoke-BrowserProcess -Executable $binary -Arguments @('--smoke-window',(Join-Path $qaDirectory 'window.png'),'--url','about:demo','--profile',(Join-Path $qaDirectory 'profile with spaces')) -LogDirectory $logs
if ($window.ExitCode -ne 0) { throw $window.Stderr }
$native = $window.Stdout | ConvertFrom-Json
if (-not $native.window_smoke -or $native.frames_presented -lt 1) { throw 'The native window did not present a frame.' }
$navigationDirectory = Join-Path $qaDirectory 'navigation fixture'
New-Item -ItemType Directory -Path $navigationDirectory -Force | Out-Null
$navigationStart = Join-Path $navigationDirectory 'index.html'
$navigationDestination = Join-Path $navigationDirectory 'second page.html'
$navigationTitle = 'Satin relative navigation OK'
$utf8 = [Text.UTF8Encoding]::new($false)
$navigationStartMarkup = @'
<!doctype html>
<title>Navigation start</title>
<p><a id="next-page" href="second%20page.html">Open the next page</a></p>
<button id="script-navigation">Open using Location</button>
<script>
  document.getElementById("script-navigation").addEventListener("click", () => {
    location.assign("second%20page.html?source=location");
  });
</script>
'@
[IO.File]::WriteAllText($navigationStart, $navigationStartMarkup, $utf8)
$navigationMarkup = @'
<!doctype html>
<title>Navigation target</title>
<main>
  <h1>Second page</h1>
  <p>This document was loaded by following a relative link.</p>
  <h2>Navigation proof</h2>
  <ol>
    <li>Read the source HTML.</li><li>Render the anchor.</li><li>Activate the link.</li>
    <li>Resolve the relative address.</li><li>Load the destination HTML.</li>
    <li>Parse the destination script.</li><li>Run the script.</li><li>Present the new document.</li>
  </ol>
</main>
<script>document.title = "Satin relative navigation OK";</script>
'@
[IO.File]::WriteAllText($navigationDestination, $navigationMarkup, $utf8)
$navigationHeadless = Invoke-BrowserProcess -Executable $binary -LogDirectory $logs -Arguments @(
    '--headless', '--url', $navigationStart, '--click-id', 'script-navigation',
    '--profile', (Join-Path $qaDirectory 'headless navigation profile')
)
if ($navigationHeadless.ExitCode -ne 0) { throw "Headless navigation failed. See $($navigationHeadless.StderrPath)." }
$navigation = $navigationHeadless.Stdout | ConvertFrom-Json
$navigationImage = Join-Path $qaDirectory 'navigation-window.png'
$navigationActions = Join-Path $qaDirectory 'navigation-actions.json'
$steps = @(@{action='click';id='next-page'}, @{action='wait_title';value=$navigationTitle;timeout_ms=5000})
[IO.File]::WriteAllText($navigationActions, ($steps | ConvertTo-Json -Compress), $utf8)
$navigationWindow = Invoke-BrowserProcess -Executable $binary -LogDirectory $logs -Arguments @(
    '--smoke-window', $navigationImage, '--smoke-actions', $navigationActions, '--url', $navigationStart,
    '--profile', (Join-Path $qaDirectory 'native navigation profile')
)
if ($navigationWindow.ExitCode -ne 0) { throw "Native navigation failed. See $($navigationWindow.StderrPath)." }
$navigationNative = $navigationWindow.Stdout | ConvertFrom-Json
foreach ($result in @($navigation, $navigationNative)) {
    if ($result.title -cne $navigationTitle -or $result.javascript.diagnostics.Count -ne 0 -or
        $result.layout_limited -or $result.paint_limited) {
        throw 'Navigation did not load and run the destination document.'
    }
}
$navigationDestinationUrl = ([Uri]$navigationDestination).AbsoluteUri
if ($navigation.javascript.executed_scripts -ne 1 -or $navigation.url -cne ($navigationDestinationUrl + '?source=location')) {
    throw 'Headless Location navigation did not preserve the query and execute the destination script.'
}
if (-not $navigationNative.window_smoke -or $navigationNative.frames_presented -lt 1 -or
    $navigationNative.url -cne $navigationDestinationUrl) {
    throw 'The native window did not present the destination after following the link.'
}
$interactive = Invoke-BrowserProcess -Executable $binary -Arguments @('--headless','--url','about:javascript','--click-id','increment','--click-count','3','--screenshot',(Join-Path $qaDirectory 'javascript.png'),'--dump-dom',(Join-Path $qaDirectory 'javascript.html')) -LogDirectory $logs
if ($interactive.ExitCode -ne 0) { throw $interactive.Stderr }
$javascript = $interactive.Stdout | ConvertFrom-Json
if ($javascript.title -ne 'Licznik: 3' -or $javascript.javascript.executed_scripts -ne 1 -or $javascript.javascript.diagnostics.Count -ne 0) { throw 'The JavaScript and DOM event check returned an unexpected result.' }
$eventWindow = Invoke-BrowserProcess -Executable $binary -Arguments @('--smoke-window',(Join-Path $qaDirectory 'javascript-window.png'),'--smoke-click-id','increment','--url','about:javascript','--profile',(Join-Path $qaDirectory 'event profile')) -LogDirectory $logs
if ($eventWindow.ExitCode -ne 0) { throw $eventWindow.Stderr }
$eventNative = $eventWindow.Stdout | ConvertFrom-Json
if ($eventNative.title -ne 'Licznik: 1' -or $eventNative.frames_presented -lt 1) { throw 'The native window did not update after the hit-tested click.' }
$timer = Invoke-BrowserProcess -Executable $binary -Arguments @('--headless','--url','about:javascript','--click-id','timer','--run-for-ms','300','--dump-dom',(Join-Path $qaDirectory 'timer.html')) -LogDirectory $logs
$timerText = '<p id="status">Timer zadzia' + [char]0x0142 + 'a' + [char]0x0142 + '.</p>'
if ($timer.ExitCode -ne 0 -or -not (Get-Content -LiteralPath (Join-Path $qaDirectory 'timer.html') -Raw -Encoding UTF8).Contains($timerText)) {
    throw 'The JavaScript timer did not update the DOM.'
}
$networkCases = @()
$platformRun = Invoke-BrowserProcess -Executable $binary -Arguments @('--headless','--url','about:platform','--click-id','add','--screenshot',(Join-Path $qaDirectory 'platform.png'),'--height','1400') -LogDirectory $logs
if ($platformRun.ExitCode -ne 0) { throw $platformRun.Stderr }
$platform = $platformRun.Stdout | ConvertFrom-Json
if ($platform.title -ne 'Platforma: 3' -or $platform.javascript.diagnostics.Count -ne 0 -or $platform.javascript.pending_requests -ne 0) { throw 'The Promise, JSON, template, and Grid check failed.' }
$formsRun = Invoke-BrowserProcess -Executable $binary -Arguments @('--headless','--url','about:forms','--click-id','send','--screenshot',(Join-Path $qaDirectory 'forms.png'),'--height','1400') -LogDirectory $logs
if ($formsRun.ExitCode -ne 0) { throw $formsRun.Stderr }
$forms = $formsRun.Stdout | ConvertFrom-Json
if ($forms.title -ne 'Formularz: OK' -or $forms.javascript.diagnostics.Count -ne 0 -or $forms.javascript.pending_requests -ne 0) { throw 'The forms, Promise.all, and JSON check failed.' }
$media = Invoke-SatinMediaSmoke -Executable $binary -ProfileDirectory (Join-Path $qaDirectory 'media profile') -OutputDirectory $qaDirectory -LogDirectory $logs
$streaming = Invoke-SatinMediaSmoke -Segmented -Executable $binary -ProfileDirectory (Join-Path $qaDirectory 'streaming profile') -OutputDirectory $qaDirectory -LogDirectory $logs
$workerHeadless = Invoke-BrowserProcess -Executable $binary -LogDirectory $logs -Arguments @(
    '--headless', '--url', 'about:workers', '--click-id', 'worker-run', '--run-for-ms', '2000'
)
if ($workerHeadless.ExitCode -ne 0) { throw $workerHeadless.Stderr }
$worker = $workerHeadless.Stdout | ConvertFrom-Json
if ($worker.title -cne 'Satin Worker OK' -or $worker.javascript.diagnostics.Count -ne 0) {
    throw 'The headless Worker and WebAssembly check failed.'
}
$workerActions = Join-Path $qaDirectory 'worker-actions.json'
[IO.File]::WriteAllText($workerActions, '[{"action":"click","id":"worker-run"},{"action":"wait_title","value":"Satin Worker OK","timeout_ms":5000}]', [Text.UTF8Encoding]::new($false))
$workerWindow = Invoke-BrowserProcess -Executable $binary -LogDirectory $logs -Arguments @(
    '--smoke-window', (Join-Path $qaDirectory 'workers-window.png'), '--smoke-actions', $workerActions,
    '--url', 'about:workers', '--profile', (Join-Path $qaDirectory 'worker profile')
)
if ($workerWindow.ExitCode -ne 0) { throw $workerWindow.Stderr }
$workerNative = $workerWindow.Stdout | ConvertFrom-Json
if (-not $workerNative.window_smoke -or $workerNative.frames_presented -lt 1 -or
    $workerNative.title -cne 'Satin Worker OK' -or $workerNative.javascript.diagnostics.Count -ne 0) {
    throw 'The native Worker and WebAssembly check failed.'
}
if ($Network) {
    $positive = Invoke-BrowserProcess -Executable $binary -Arguments @('--headless','--url','https://example.com','--screenshot',(Join-Path $qaDirectory 'https-example.png')) -LogDirectory $logs
    if ($positive.ExitCode -ne 0) { throw ('The valid HTTPS request failed. ' + $positive.Stderr) }
    $https = $positive.Stdout | ConvertFrom-Json
    if ($https.title -ne 'Example Domain') { throw 'The valid HTTPS request returned an unexpected page title.' }
    $networkCases += [pscustomobject]@{url='https://example.com';result='accepted';title=$https.title}
    foreach ($bad in @('https://expired.badssl.com/','https://self-signed.badssl.com/')) {
        $negative = Invoke-BrowserProcess -Executable $binary -Arguments @('--headless','--url',$bad) -LogDirectory $logs
        if ($negative.ExitCode -eq 0 -or $negative.Stderr -notmatch 'certificate|certyfikat|Cert') { throw ("The invalid certificate check failed for $bad. " + $negative.Stderr) }
        $networkCases += [pscustomobject]@{url=$bad;result='rejected';error=$negative.Stderr.Trim()}
    }
}
$report = [ordered]@{
    version=$version.Stdout.Trim();document=$document;nativeWindow=$native
    navigation=$navigation;navigationNative=$navigationNative
    javascript=$javascript;platform=$platform;forms=$forms;eventWindow=$eventNative;timer='passed'
    network=$networkCases;media=$media;streaming=$streaming;worker=$worker;workerNative=$workerNative
}
[IO.File]::WriteAllText((Join-Path $qaDirectory 'report.json'),($report | ConvertTo-Json -Depth 10),[Text.UTF8Encoding]::new($false))
$report | ConvertTo-Json -Depth 10
