param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$projectDirectory = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
. (Join-Path $PSScriptRoot 'Process.ps1')
$processLogs = Join-Path $projectDirectory 'target\install-logs'
if (-not $SkipBuild) { & (Join-Path $PSScriptRoot 'Build.ps1') -Release -Test }
$binary = Join-Path $projectDirectory 'target\release\rust-browser.exe'
$launcher = Join-Path $projectDirectory 'target\release\rust-browser-launcher.exe'
if (-not (Test-Path -LiteralPath $binary) -or -not (Test-Path -LiteralPath $launcher)) { throw 'Brak skompilowanej wersji release.' }
$installRoot = Join-Path $env:LOCALAPPDATA 'RustBrowser'
$versionCheck = Invoke-BrowserProcess -Executable $binary -Arguments @('--version') -LogDirectory $processLogs
if ($versionCheck.ExitCode -ne 0) { throw $versionCheck.Stderr }
$versionOutput = $versionCheck.Stdout.Trim()
if ($versionOutput -notmatch '^Rust Browser ([0-9][^\r\n]+)$') { throw 'Nie można ustalić wersji pliku wykonywalnego.' }
$version = $Matches[1].Trim()
$current = Join-Path $installRoot 'current.json'
$desktop = [Environment]::GetFolderPath('DesktopDirectory')
$shortcutPath = Join-Path $desktop 'Rust Browser.lnk'
$installedLauncher = Join-Path $installRoot 'RustBrowser.exe'
$shell = New-Object -ComObject WScript.Shell
if (Test-Path -LiteralPath $current) {
    $old = Get-Content -LiteralPath $current -Raw | ConvertFrom-Json
    if ($old.product -ne 'RustBrowser') { throw 'Istniejący katalog nie należy do tej instalacji.' }
}
if (Test-Path -LiteralPath $shortcutPath) {
    $oldShortcut = $shell.CreateShortcut($shortcutPath)
    if ($oldShortcut.TargetPath -ne $installedLauncher) { throw 'Skrót o tej nazwie prowadzi do innej aplikacji; zachowano go.' }
}
$buildId = $version + '-' + [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss-fff')
$versionRoot = Join-Path $installRoot ('versions\' + $buildId)
New-Item -ItemType Directory -Path $versionRoot -Force | Out-Null
Copy-Item -LiteralPath $binary -Destination (Join-Path $versionRoot 'rust-browser.exe')
$testProfile = Join-Path $projectDirectory 'target\install-check'
$smokeImage = Join-Path $projectDirectory 'target\install-check.png'
$candidate = Join-Path $versionRoot 'rust-browser.exe'
$smokeCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:demo','--screenshot',$smokeImage,'--profile',$testProfile) -LogDirectory $processLogs
if ($smokeCheck.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $smokeImage)) { throw ('Test zainstalowanej kopii nie przeszedł; aktywna wersja pozostaje bez zmian. ' + $smokeCheck.Stderr) }
$smoke = $smokeCheck.Stdout | ConvertFrom-Json
if ($smoke.version -ne $version -or $smoke.commands -lt 10) { throw 'Nieoczekiwany wynik próby uruchomienia.' }
$scriptCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:javascript','--click-id','increment','--click-count','2') -LogDirectory $processLogs
if ($scriptCheck.ExitCode -ne 0) { throw ('Test JavaScript kandydata nie przeszedł: ' + $scriptCheck.Stderr) }
$scriptSmoke = $scriptCheck.Stdout | ConvertFrom-Json
if ($scriptSmoke.title -ne 'Licznik: 2' -or $scriptSmoke.javascript.executed_scripts -ne 1 -or $scriptSmoke.javascript.diagnostics.Count -ne 0) { throw 'Niepoprawne działanie DOM/JavaScript kandydata; aktywna wersja pozostaje bez zmian.' }
$moduleTitle = 'Modu' + [char]0x0142 + 'y: 1'
$moduleCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:modules','--click-id','module-increment') -LogDirectory $processLogs
if ($moduleCheck.ExitCode -ne 0) { throw ('Test modulu kandydata nie przeszedl: ' + $moduleCheck.Stderr) }
$moduleSmoke = $moduleCheck.Stdout | ConvertFrom-Json
if ($moduleSmoke.title -ne $moduleTitle -or $moduleSmoke.javascript.executed_scripts -ne 2 -or $moduleSmoke.javascript.diagnostics.Count -ne 0) { throw 'Niepoprawny wynik proby parsera i modulow JavaScript.' }
$asyncCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:async','--click-id','run-async','--run-for-ms','1600','--profile',$testProfile) -LogDirectory $processLogs
if ($asyncCheck.ExitCode -ne 0) { throw ('Proba asynchroniczna kandydata nie przeszla. ' + $asyncCheck.Stderr) }
$asyncSmoke = $asyncCheck.Stdout | ConvertFrom-Json
if ($asyncSmoke.title -ne 'Zadanie: OK' -or $asyncSmoke.javascript.executed_scripts -ne 2 -or $asyncSmoke.javascript.pending_requests -ne 0 -or $asyncSmoke.javascript.diagnostics.Count -ne 0) { throw 'Proba async/await, Fetch i skryptu dolaczonego po wczytaniu zwrocila niepoprawny wynik.' }
$routeDetails = 'Trasa: Szczeg' + [char]0x00F3 + [char]0x0142 + 'y'
$routeStart = 'Trasa: Pocz' + [char]0x0105 + 'tek'
$routeCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:routes','--click-id','route-details','--profile',$testProfile) -LogDirectory $processLogs
if ($routeCheck.ExitCode -ne 0) { throw ('Proba historii kandydata nie przeszla. ' + $routeCheck.Stderr) }
$routeSmoke = $routeCheck.Stdout | ConvertFrom-Json
if ($routeSmoke.title -ne $routeDetails -or $routeSmoke.url -ne 'about:routes#details' -or $routeSmoke.javascript.diagnostics.Count -ne 0) { throw 'Niepoprawny wynik proby History, MutationObserver, Map i RegExp.' }
$bytesCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:bytes','--click-id','binary-run','--profile',$testProfile) -LogDirectory $processLogs
if ($bytesCheck.ExitCode -ne 0) { throw ('Próba danych binarnych nie przeszła. ' + $bytesCheck.Stderr) }
$bytesSmoke = $bytesCheck.Stdout | ConvertFrom-Json
if ($bytesSmoke.title -ne 'Dane binarne OK' -or $bytesSmoke.javascript.diagnostics.Count -ne 0) { throw 'Niepoprawny wynik odczytu strumienia lub formularza multipart.' }
$timeDom = Join-Path $projectDirectory 'target\install-time-check.html'
$timeCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:time','--click-id','start-time','--run-for-ms','500','--dump-dom',$timeDom,'--profile',$testProfile) -LogDirectory $processLogs
if ($timeCheck.ExitCode -ne 0) { throw ('Próba zegara nie przeszła. ' + $timeCheck.Stderr) }
$timeSmoke = $timeCheck.Stdout | ConvertFrom-Json
if ($timeSmoke.title -ne 'Zegar aktywny' -or $timeSmoke.javascript.diagnostics.Count -ne 0 -or (Get-Content -LiteralPath $timeDom -Raw) -notmatch 'data-sampled="true"') { throw 'Zegar nie odczytał czasu i znacznika klatki.' }
$canvasTitle = 'P' + [char]0x0142 + [char]0x00F3 + 'tno gotowe'
$canvasDom = Join-Path $projectDirectory 'target\install-canvas-check.html'
$canvasCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:canvas','--click-id','draw-canvas','--dump-dom',$canvasDom,'--profile',$testProfile) -LogDirectory $processLogs
if ($canvasCheck.ExitCode -ne 0) { throw ('Proba Canvas 2D nie przeszla. ' + $canvasCheck.Stderr) }
$canvasSmoke = $canvasCheck.Stdout | ConvertFrom-Json
if ($canvasSmoke.title -ne $canvasTitle -or $canvasSmoke.javascript.diagnostics.Count -ne 0 -or (Get-Content -LiteralPath $canvasDom -Raw) -notmatch 'RGBA\(24, 131, 111, 255\)') { throw 'Canvas 2D zwrocil niepoprawny odczyt piksela.' }
$pipeDom = Join-Path $projectDirectory 'target\install-pipe-check.html'
$pipeCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:bytes','--click-id','binary-pipe','--run-for-ms','650','--dump-dom',$pipeDom,'--profile',$testProfile) -LogDirectory $processLogs
if ($pipeCheck.ExitCode -ne 0) { throw ('Proba potoku nie przeszla. ' + $pipeCheck.Stderr) }
$pipeSmoke = $pipeCheck.Stdout | ConvertFrom-Json
if ($pipeSmoke.title -ne 'Potok gotowy' -or $pipeSmoke.javascript.diagnostics.Count -ne 0 -or (Get-Content -LiteralPath $pipeDom -Raw) -notmatch 'Odczyt BYOB: 8 B') { throw 'Potok nie odczytal, nie przeniosl lub nie przeksztalcil danych.' }
$numberDom = Join-Path $projectDirectory 'target\install-numbers-check.html'
$numberCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:numbers','--click-id','number-run','--dump-dom',$numberDom,'--profile',$testProfile) -LogDirectory $processLogs
if ($numberCheck.ExitCode -ne 0) { throw ('Proba duzych liczb nie przeszla. ' + $numberCheck.Stderr) }
$numberSmoke = $numberCheck.Stdout | ConvertFrom-Json
$numberText = Get-Content -LiteralPath $numberDom -Raw
if ($numberSmoke.title -ne 'Liczby gotowe' -or $numberSmoke.javascript.diagnostics.Count -ne 0 -or $numberText -notmatch 'data-verified="yes"' -or $numberText -notmatch '340282366920938463426481119284349108225' -or $numberText -notmatch 'ff ff ff ff ff ff ff ff') { throw 'Proba BigInt, akcesorow lub danych 64-bitowych zwrocila niepoprawny wynik.' }
$messageTitle = 'Odebrano 42'
$messageDom = Join-Path $projectDirectory 'target\install-messages-check.html'
$messageCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:messages','--click-id','message-run','--run-for-ms','650','--dump-dom',$messageDom,'--profile',$testProfile) -LogDirectory $processLogs
if ($messageCheck.ExitCode -ne 0) { throw ('Proba portow wiadomosci nie przeszla. ' + $messageCheck.Stderr) }
$messageSmoke = $messageCheck.Stdout | ConvertFrom-Json
if ($messageSmoke.title -ne $messageTitle -or $messageSmoke.javascript.diagnostics.Count -ne 0 -or (Get-Content -LiteralPath $messageDom -Raw) -notmatch '>Odebrano 42\. Bufor') { throw 'Porty nie odeslaly wyniku po przeniesieniu bufora.' }
$fileTitle = 'Podgl' + [char]0x0105 + 'd gotowy'
$fileSample = Join-Path $processLogs 'probe-image.png'
[IO.File]::WriteAllBytes($fileSample, [Convert]::FromBase64String('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNg+M/wHwAEAQH/cetH5QAAAABJRU5ErkJggg=='))
$fileAnswers = Join-Path $processLogs 'file-picker-responses.json'
$fileSelections = ,([string[]]@($fileSample))
[IO.File]::WriteAllText($fileAnswers, (ConvertTo-Json -InputObject $fileSelections -Compress), [Text.UTF8Encoding]::new($false))
$fileDom = Join-Path $projectDirectory 'target\install-files-check.html'
$fileCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:files','--click-id','image-pick','--file-picker-responses',$fileAnswers,'--run-for-ms','650','--dump-dom',$fileDom,'--profile',$testProfile) -LogDirectory $processLogs
if ($fileCheck.ExitCode -ne 0) { throw ('Proba wyboru pliku i dekodowania obrazu nie przeszla. ' + $fileCheck.Stderr) }
$fileSmoke = $fileCheck.Stdout | ConvertFrom-Json
$fileText = Get-Content -LiteralPath $fileDom -Raw
if ($fileSmoke.title -ne $fileTitle -or $fileSmoke.javascript.diagnostics.Count -ne 0 -or $fileText -notmatch '>probe-image\.png' -or $fileText -notmatch '<canvas\b(?=[^>]*\bwidth="300")(?=[^>]*\bheight="300")[^>]*>') { throw 'Wybrany plik nie pojawil sie na plotnie.' }
$graphicsCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:graphics','--click-id','move','--click-count','2') -LogDirectory $processLogs
if ($graphicsCheck.ExitCode -ne 0) { throw ('Test grafiki i pomiarów kandydata nie przeszedł: ' + $graphicsCheck.Stderr) }
$graphicsSmoke = $graphicsCheck.Stdout | ConvertFrom-Json
if ($graphicsSmoke.title -ne 'Grafika: 2' -or $graphicsSmoke.javascript.diagnostics.Count -ne 0 -or $graphicsSmoke.layout_limited -or $graphicsSmoke.svg.scenes -lt 1) { throw 'Niepoprawny wynik próby CSSOM/SVG/składni JavaScript.' }
$windowImage = Join-Path $projectDirectory 'target\install-window-check.png'
$windowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$windowImage,'--smoke-click-id','send','--url','about:forms','--profile',$testProfile) -LogDirectory $processLogs
if ($windowCheck.ExitCode -ne 0) { throw ('Test natywnego okna nie przeszedł; aktywna wersja pozostaje bez zmian. ' + $windowCheck.Stderr) }
$windowSmoke = $windowCheck.Stdout | ConvertFrom-Json
if (-not $windowSmoke.window_smoke -or $windowSmoke.frames_presented -lt 1 -or $windowSmoke.title -ne 'Formularz: OK') { throw 'Okno nie przedstawiło klatki z poprawnie obsłużonym formularzem/Promise.all/JSON.' }
$graphicsImage = Join-Path $projectDirectory 'target\install-graphics-check.png'
$graphicsWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$graphicsImage,'--smoke-click-id','click-shape','--url','about:graphics','--profile',$testProfile) -LogDirectory $processLogs
if ($graphicsWindowCheck.ExitCode -ne 0) { throw ('Test kliknięcia SVG nie przeszedł: ' + $graphicsWindowCheck.Stderr) }
$graphicsWindowSmoke = $graphicsWindowCheck.Stdout | ConvertFrom-Json
if (-not $graphicsWindowSmoke.window_smoke -or $graphicsWindowSmoke.frames_presented -lt 1 -or $graphicsWindowSmoke.title -ne 'SVG: OK' -or $graphicsWindowSmoke.paint_limited -or $graphicsWindowSmoke.layout_limited -or $graphicsWindowSmoke.javascript.diagnostics.Count -ne 0) { throw 'Okno nie przedstawiło klatki z poprawnie obsłużonym kliknięciem SVG.' }
$moduleImage = Join-Path $projectDirectory 'target\install-modules-check.png'
$moduleWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$moduleImage,'--smoke-click-id','module-increment','--url','about:modules','--profile',$testProfile) -LogDirectory $processLogs
if ($moduleWindowCheck.ExitCode -ne 0) { throw ('Test okna z modulem JavaScript nie przeszedl: ' + $moduleWindowCheck.Stderr) }
$moduleWindowSmoke = $moduleWindowCheck.Stdout | ConvertFrom-Json
if (-not $moduleWindowSmoke.window_smoke -or $moduleWindowSmoke.frames_presented -lt 1 -or $moduleWindowSmoke.title -ne $moduleTitle -or $moduleWindowSmoke.javascript.diagnostics.Count -ne 0) { throw 'Okno nie przedstawilo klatki po wykonaniu modulu i kliknieciu.' }
$asyncImage = Join-Path $projectDirectory 'target\install-async-check.png'
$asyncActions = Join-Path $processLogs 'async-actions.json'
[IO.File]::WriteAllText($asyncActions, '[{"action":"click","id":"run-async"},{"action":"click","id":"cancel-async"}]', [Text.UTF8Encoding]::new($false))
$asyncWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$asyncImage,'--smoke-actions',$asyncActions,'--url','about:async','--profile',$testProfile) -LogDirectory $processLogs
if ($asyncWindowCheck.ExitCode -ne 0) { throw ('Proba anulowania w oknie nie przeszla. ' + $asyncWindowCheck.Stderr) }
$asyncWindowSmoke = $asyncWindowCheck.Stdout | ConvertFrom-Json
if (-not $asyncWindowSmoke.window_smoke -or $asyncWindowSmoke.frames_presented -lt 1 -or $asyncWindowSmoke.title -ne 'Zadanie: anulowano' -or $asyncWindowSmoke.javascript.diagnostics.Count -ne 0) { throw 'Okno nie przedstawilo klatki z anulowanym zadaniem.' }
$routeImage = Join-Path $projectDirectory 'target\install-routes-check.png'
$routeActions = Join-Path $processLogs 'routes-actions.json'
$routeSteps = @(
    @{action='click';id='route-details'},
    @{action='title';value=$routeDetails},
    @{action='key';value='ArrowLeft';alt=$true},
    @{action='title';value=$routeStart},
    @{action='key';value='ArrowRight';alt=$true},
    @{action='title';value=$routeDetails}
)
[IO.File]::WriteAllText($routeActions, ($routeSteps | ConvertTo-Json -Compress), [Text.UTF8Encoding]::new($false))
$routeWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$routeImage,'--smoke-actions',$routeActions,'--url','about:routes','--profile',$testProfile) -LogDirectory $processLogs
if ($routeWindowCheck.ExitCode -ne 0) { throw ('Proba nawigacji w oknie nie przeszla. ' + $routeWindowCheck.Stderr) }
$routeWindowSmoke = $routeWindowCheck.Stdout | ConvertFrom-Json
if (-not $routeWindowSmoke.window_smoke -or $routeWindowSmoke.frames_presented -lt 1 -or $routeWindowSmoke.url -ne 'about:routes#details' -or $routeWindowSmoke.javascript.diagnostics.Count -ne 0) { throw 'Okno nie przedstawilo klatki po powrocie i przejsciu dalej.' }
$bytesImage = Join-Path $projectDirectory 'target\install-bytes-check.png'
$bytesWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$bytesImage,'--smoke-click-id','binary-run','--url','about:bytes','--profile',$testProfile) -LogDirectory $processLogs
if ($bytesWindowCheck.ExitCode -ne 0) { throw ('Próba plików i strumieni w oknie nie przeszła. ' + $bytesWindowCheck.Stderr) }
$bytesWindowSmoke = $bytesWindowCheck.Stdout | ConvertFrom-Json
if (-not $bytesWindowSmoke.window_smoke -or $bytesWindowSmoke.frames_presented -lt 1 -or $bytesWindowSmoke.title -ne 'Dane binarne OK' -or $bytesWindowSmoke.javascript.diagnostics.Count -ne 0 -or $bytesWindowSmoke.layout_limited -or $bytesWindowSmoke.paint_limited) { throw 'Okno nie przedstawiło wyniku odczytu bajtów i formularza.' }
$timeImage = Join-Path $projectDirectory 'target\install-time-window.png'
$timeWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$timeImage,'--smoke-click-id','start-time','--url','about:time','--profile',$testProfile) -LogDirectory $processLogs
if ($timeWindowCheck.ExitCode -ne 0) { throw ('Próba zegara w oknie nie przeszła. ' + $timeWindowCheck.Stderr) }
$timeWindowSmoke = $timeWindowCheck.Stdout | ConvertFrom-Json
if (-not $timeWindowSmoke.window_smoke -or $timeWindowSmoke.frames_presented -lt 1 -or $timeWindowSmoke.title -ne 'Zegar aktywny' -or $timeWindowSmoke.javascript.diagnostics.Count -ne 0 -or $timeWindowSmoke.layout_limited -or $timeWindowSmoke.paint_limited) { throw 'Okno nie przedstawiło widoku działającego zegara.' }
$canvasImage = Join-Path $projectDirectory 'target\install-canvas-window.png'
$canvasWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$canvasImage,'--smoke-click-id','draw-canvas','--url','about:canvas','--profile',$testProfile) -LogDirectory $processLogs
if ($canvasWindowCheck.ExitCode -ne 0) { throw ('Proba plotna w oknie nie przeszla. ' + $canvasWindowCheck.Stderr) }
$canvasWindowSmoke = $canvasWindowCheck.Stdout | ConvertFrom-Json
if (-not $canvasWindowSmoke.window_smoke -or $canvasWindowSmoke.frames_presented -lt 1 -or $canvasWindowSmoke.title -ne $canvasTitle -or $canvasWindowSmoke.javascript.diagnostics.Count -ne 0 -or $canvasWindowSmoke.layout_limited -or $canvasWindowSmoke.paint_limited) { throw 'Okno nie przedstawilo narysowanego plotna.' }
$pipeImage = Join-Path $projectDirectory 'target\install-pipe-window.png'
$pipeActions = Join-Path $processLogs 'pipe-actions.json'
[IO.File]::WriteAllText($pipeActions, '[{"action":"click","id":"binary-pipe"},{"action":"title","value":"Potok gotowy"}]', [Text.UTF8Encoding]::new($false))
$pipeWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$pipeImage,'--smoke-actions',$pipeActions,'--url','about:bytes','--profile',$testProfile) -LogDirectory $processLogs
if ($pipeWindowCheck.ExitCode -ne 0) { throw ('Proba potoku w oknie nie przeszla. ' + $pipeWindowCheck.Stderr) }
$pipeWindowSmoke = $pipeWindowCheck.Stdout | ConvertFrom-Json
if (-not $pipeWindowSmoke.window_smoke -or $pipeWindowSmoke.frames_presented -lt 1 -or $pipeWindowSmoke.title -ne 'Potok gotowy' -or $pipeWindowSmoke.javascript.diagnostics.Count -ne 0) { throw 'Okno nie przedstawilo wyniku potoku.' }
$numberImage = Join-Path $projectDirectory 'target\install-numbers-window.png'
$numberWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$numberImage,'--smoke-click-id','number-run','--url','about:numbers','--profile',$testProfile) -LogDirectory $processLogs
if ($numberWindowCheck.ExitCode -ne 0) { throw ('Proba duzych liczb w oknie nie przeszla. ' + $numberWindowCheck.Stderr) }
$numberWindowSmoke = $numberWindowCheck.Stdout | ConvertFrom-Json
if (-not $numberWindowSmoke.window_smoke -or $numberWindowSmoke.frames_presented -lt 1 -or $numberWindowSmoke.title -ne 'Liczby gotowe' -or $numberWindowSmoke.javascript.diagnostics.Count -ne 0 -or $numberWindowSmoke.layout_limited -or $numberWindowSmoke.paint_limited) { throw 'Okno nie przedstawilo wyniku obliczenia BigInt.' }
$messageImage = Join-Path $projectDirectory 'target\install-messages-window.png'
$messageActions = Join-Path $processLogs 'messages-actions.json'
[IO.File]::WriteAllText($messageActions, '[{"action":"click","id":"message-run"},{"action":"title","value":"Odebrano 42"}]', [Text.UTF8Encoding]::new($false))
$messageWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$messageImage,'--smoke-actions',$messageActions,'--url','about:messages','--profile',$testProfile) -LogDirectory $processLogs
if ($messageWindowCheck.ExitCode -ne 0) { throw ('Proba portow wiadomosci w oknie nie przeszla. ' + $messageWindowCheck.Stderr) }
$messageWindowSmoke = $messageWindowCheck.Stdout | ConvertFrom-Json
if (-not $messageWindowSmoke.window_smoke -or $messageWindowSmoke.frames_presented -lt 1 -or $messageWindowSmoke.title -ne $messageTitle -or $messageWindowSmoke.javascript.diagnostics.Count -ne 0) { throw 'Okno nie przedstawilo odpowiedzi MessagePort.' }
$fileImage = Join-Path $projectDirectory 'target\install-files-window.png'
$fileActions = Join-Path $processLogs 'files-actions.json'
$fileActionSteps = @(@{action='click';id='image-pick'}, @{action='title';value=$fileTitle})
[IO.File]::WriteAllText($fileActions, (ConvertTo-Json -InputObject $fileActionSteps -Compress), [Text.UTF8Encoding]::new($false))
$fileWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$fileImage,'--smoke-actions',$fileActions,'--url','about:files','--file-picker-responses',$fileAnswers,'--profile',$testProfile) -LogDirectory $processLogs
if ($fileWindowCheck.ExitCode -ne 0) { throw ('Proba podgladu pliku w oknie nie przeszla. ' + $fileWindowCheck.Stderr) }
$fileWindowSmoke = $fileWindowCheck.Stdout | ConvertFrom-Json
if (-not $fileWindowSmoke.window_smoke -or $fileWindowSmoke.frames_presented -lt 1 -or $fileWindowSmoke.title -ne $fileTitle -or $fileWindowSmoke.javascript.diagnostics.Count -ne 0) { throw 'Okno nie przedstawilo podgladu wybranego pliku.' }
$pendingLauncher = Join-Path $installRoot 'RustBrowser.pending.exe'
Copy-Item -LiteralPath $launcher -Destination $pendingLauncher -Force
if (Test-Path -LiteralPath $installedLauncher) {
    [IO.File]::Replace($pendingLauncher,$installedLauncher,(Join-Path $installRoot 'RustBrowser.previous.exe'))
} else {
    [IO.File]::Move($pendingLauncher,$installedLauncher)
}
$manifest = [ordered]@{
    product = 'RustBrowser'
    version = $version
    buildId = $buildId
    executable = 'versions/' + $buildId + '/rust-browser.exe'
    sha256 = (Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash
    profileSchema = 1
}
$pending = Join-Path $installRoot 'current.pending'
$previous = Join-Path $installRoot 'previous.json'
[IO.File]::WriteAllText($pending, ($manifest | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
if (Test-Path -LiteralPath $current) {
    $old = Get-Content -LiteralPath $current -Raw | ConvertFrom-Json
    if ($old.product -ne 'RustBrowser') { throw 'Istniejący katalog nie należy do tej instalacji.' }
    [IO.File]::Replace($pending, $current, $previous)
} else { [IO.File]::Move($pending, $current) }
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $installedLauncher
$shortcut.WorkingDirectory = $installRoot
$shortcut.IconLocation = $installedLauncher + ',0'
$shortcut.Description = 'Rust Browser — aktualna wersja rozwojowa'
$shortcut.Save()
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'Rollback.ps1') -Destination (Join-Path $installRoot 'Rollback.ps1') -Force
[ordered]@{version=$version;buildId=$buildId;shortcut=$shortcutPath;installation=$installRoot;smoke=$smoke;javascript=$scriptSmoke;graphics=$graphicsSmoke;windowSmoke=$windowSmoke;graphicsWindowSmoke=$graphicsWindowSmoke;modules=$moduleSmoke;moduleWindowSmoke=$moduleWindowSmoke;async=$asyncSmoke;asyncWindowSmoke=$asyncWindowSmoke;routes=$routeSmoke;routeWindowSmoke=$routeWindowSmoke;bytes=$bytesSmoke;bytesWindowSmoke=$bytesWindowSmoke;time=$timeSmoke;timeWindowSmoke=$timeWindowSmoke;canvas=$canvasSmoke;canvasWindowSmoke=$canvasWindowSmoke;pipe=$pipeSmoke;pipeWindowSmoke=$pipeWindowSmoke;numbers=$numberSmoke;numbersWindowSmoke=$numberWindowSmoke;messages=$messageSmoke;messagesWindowSmoke=$messageWindowSmoke;files=$fileSmoke;filesWindowSmoke=$fileWindowSmoke} | ConvertTo-Json -Depth 5
