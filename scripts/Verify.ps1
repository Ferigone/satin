param([switch]$Network)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Process.ps1')
$projectDirectory = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$binary = Join-Path $projectDirectory 'target\release\rust-browser.exe'
$qaDirectory = Join-Path $projectDirectory 'target\verification'
$logs = Join-Path $qaDirectory 'logs'
New-Item -ItemType Directory -Path $qaDirectory -Force | Out-Null
$version = Invoke-BrowserProcess -Executable $binary -Arguments @('--version') -LogDirectory $logs
if ($version.ExitCode -ne 0 -or $version.Stdout -notmatch 'Rust Browser [0-9]') { throw 'Brak poprawnej wersji programu.' }
$render = Invoke-BrowserProcess -Executable $binary -Arguments @('--headless','--url','about:demo','--screenshot',(Join-Path $qaDirectory 'demo.png'),'--dump-dom',(Join-Path $qaDirectory 'demo.html'),'--height','1600') -LogDirectory $logs
if ($render.ExitCode -ne 0) { throw $render.Stderr }
$document = $render.Stdout | ConvertFrom-Json
if ($document.commands -lt 20 -or $document.links -lt 3) { throw 'Niepełny wynik renderowania strony próbnej.' }
$window = Invoke-BrowserProcess -Executable $binary -Arguments @('--smoke-window',(Join-Path $qaDirectory 'window.png'),'--url','about:demo','--profile',(Join-Path $qaDirectory 'profile with spaces')) -LogDirectory $logs
if ($window.ExitCode -ne 0) { throw $window.Stderr }
$native = $window.Stdout | ConvertFrom-Json
if (-not $native.window_smoke -or $native.frames_presented -lt 1) { throw 'Próba natywnego okna nie przedstawiła klatki.' }
$interactive = Invoke-BrowserProcess -Executable $binary -Arguments @('--headless','--url','about:javascript','--click-id','increment','--click-count','3','--screenshot',(Join-Path $qaDirectory 'javascript.png'),'--dump-dom',(Join-Path $qaDirectory 'javascript.html')) -LogDirectory $logs
if ($interactive.ExitCode -ne 0) { throw $interactive.Stderr }
$javascript = $interactive.Stdout | ConvertFrom-Json
if ($javascript.title -ne 'Licznik: 3' -or $javascript.javascript.executed_scripts -ne 1 -or $javascript.javascript.diagnostics.Count -ne 0) { throw 'Niepoprawny wynik własnego JavaScript i zdarzeń DOM.' }
$eventWindow = Invoke-BrowserProcess -Executable $binary -Arguments @('--smoke-window',(Join-Path $qaDirectory 'javascript-window.png'),'--smoke-click-id','increment','--url','about:javascript','--profile',(Join-Path $qaDirectory 'event profile')) -LogDirectory $logs
if ($eventWindow.ExitCode -ne 0) { throw $eventWindow.Stderr }
$eventNative = $eventWindow.Stdout | ConvertFrom-Json
if ($eventNative.title -ne 'Licznik: 1' -or $eventNative.frames_presented -lt 1) { throw 'Ścieżka kliknięcia po geometrii nie zaktualizowała natywnego okna.' }
$timer = Invoke-BrowserProcess -Executable $binary -Arguments @('--headless','--url','about:javascript','--click-id','timer','--run-for-ms','300','--dump-dom',(Join-Path $qaDirectory 'timer.html')) -LogDirectory $logs
if ($timer.ExitCode -ne 0 -or (Get-Content -LiteralPath (Join-Path $qaDirectory 'timer.html') -Raw) -notmatch '<p id="status">Timer zadziałał.</p>') { throw 'Timer JavaScript nie zaktualizował DOM.' }
$networkCases = @()
$platformRun = Invoke-BrowserProcess -Executable $binary -Arguments @('--headless','--url','about:platform','--click-id','add','--screenshot',(Join-Path $qaDirectory 'platform.png'),'--height','1400') -LogDirectory $logs
if ($platformRun.ExitCode -ne 0) { throw $platformRun.Stderr }
$platform = $platformRun.Stdout | ConvertFrom-Json
if ($platform.title -ne 'Platforma: 3' -or $platform.javascript.diagnostics.Count -ne 0 -or $platform.javascript.pending_requests -ne 0) { throw 'Platforma Promise/JSON/template/Grid nie przeszła testu.' }
$formsRun = Invoke-BrowserProcess -Executable $binary -Arguments @('--headless','--url','about:forms','--click-id','send','--screenshot',(Join-Path $qaDirectory 'forms.png'),'--height','1400') -LogDirectory $logs
if ($formsRun.ExitCode -ne 0) { throw $formsRun.Stderr }
$forms = $formsRun.Stdout | ConvertFrom-Json
if ($forms.title -ne 'Formularz: OK' -or $forms.javascript.diagnostics.Count -ne 0 -or $forms.javascript.pending_requests -ne 0) { throw 'Formularz/Promise.all/JSON nie przeszedł testu.' }
if ($Network) {
    $positive = Invoke-BrowserProcess -Executable $binary -Arguments @('--headless','--url','https://example.com','--screenshot',(Join-Path $qaDirectory 'https-example.png')) -LogDirectory $logs
    if ($positive.ExitCode -ne 0) { throw ('Poprawny HTTPS nie działa: ' + $positive.Stderr) }
    $https = $positive.Stdout | ConvertFrom-Json
    if ($https.title -ne 'Example Domain') { throw 'Strona HTTPS ma nieoczekiwany tytuł.' }
    $networkCases += [pscustomobject]@{url='https://example.com';result='accepted';title=$https.title}
    foreach ($bad in @('https://expired.badssl.com/','https://self-signed.badssl.com/')) {
        $negative = Invoke-BrowserProcess -Executable $binary -Arguments @('--headless','--url',$bad) -LogDirectory $logs
        if ($negative.ExitCode -eq 0 -or $negative.Stderr -notmatch 'certificate|certyfikat|Cert') { throw ('Nieprawidłowy wynik weryfikacji certyfikatu: ' + $bad + ' ' + $negative.Stderr) }
        $networkCases += [pscustomobject]@{url=$bad;result='rejected';error=$negative.Stderr.Trim()}
    }
}
$report = [ordered]@{version=$version.Stdout.Trim();document=$document;nativeWindow=$native;javascript=$javascript;platform=$platform;forms=$forms;eventWindow=$eventNative;timer='passed';network=$networkCases}
[IO.File]::WriteAllText((Join-Path $qaDirectory 'report.json'),($report | ConvertTo-Json -Depth 10),[Text.UTF8Encoding]::new($false))
$report | ConvertTo-Json -Depth 10
