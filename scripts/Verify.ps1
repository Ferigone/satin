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
$report = [ordered]@{version=$version.Stdout.Trim();document=$document;nativeWindow=$native;javascript=$javascript;platform=$platform;forms=$forms;eventWindow=$eventNative;timer='passed';network=$networkCases;media=$media;streaming=$streaming}
[IO.File]::WriteAllText((Join-Path $qaDirectory 'report.json'),($report | ConvertTo-Json -Depth 10),[Text.UTF8Encoding]::new($false))
$report | ConvertTo-Json -Depth 10
