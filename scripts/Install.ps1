param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$projectDirectory = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
. (Join-Path $PSScriptRoot 'Process.ps1')
$runId = [Guid]::NewGuid().ToString('N')
$qaDirectory = Join-Path $projectDirectory ('target\install-checks\' + $runId)
$processLogs = Join-Path $qaDirectory 'logs'
if (-not $SkipBuild) { & (Join-Path $PSScriptRoot 'Build.ps1') -Release -Test }
$binary = Join-Path $projectDirectory 'target\release\rust-browser.exe'
$launcher = Join-Path $projectDirectory 'target\release\rust-browser-launcher.exe'
if (-not (Test-Path -LiteralPath $binary) -or -not (Test-Path -LiteralPath $launcher)) { throw 'The Satin release binaries are missing.' }
$installRoot = Join-Path $env:LOCALAPPDATA 'RustBrowser'
$versionCheck = Invoke-BrowserProcess -Executable $binary -Arguments @('--version') -LogDirectory $processLogs
if ($versionCheck.ExitCode -ne 0) { throw "The Satin executable version check failed. See $($versionCheck.StderrPath)." }
$versionOutput = $versionCheck.Stdout.Trim()
if ($versionOutput -cnotmatch '^Satin ([0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)$' -or $Matches[1].Length -gt 120) {
    throw 'The Satin executable returned an invalid version.'
}
$version = $Matches[1]
$current = Join-Path $installRoot 'current.json'
$desktop = [Environment]::GetFolderPath('DesktopDirectory')
$shortcutPath = Join-Path $desktop 'Satin.lnk'
$installedLauncher = Join-Path $installRoot 'RustBrowser.exe'
$shell = New-Object -ComObject WScript.Shell
if (Test-Path -LiteralPath $current) {
    $null = ConvertFrom-SatinManifest ([IO.File]::ReadAllText($current))
}
if (Test-Path -LiteralPath $shortcutPath) {
    $oldShortcut = $shell.CreateShortcut($shortcutPath)
    if ($oldShortcut.TargetPath -ne $installedLauncher) { throw 'The existing Satin shortcut points to a different application.' }
}
$buildId = $version + '-' + [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss-fff') + '-' + $runId.Substring(0, 16)
$versionRoot = Join-Path $installRoot ('versions\' + $buildId)
New-Item -ItemType Directory -Path $versionRoot -Force | Out-Null
Copy-Item -LiteralPath $binary -Destination (Join-Path $versionRoot 'rust-browser.exe')
$testProfile = Join-Path $qaDirectory 'profile'
$smokeImage = Join-Path $qaDirectory 'install-check.png'
$candidate = Join-Path $versionRoot 'rust-browser.exe'
$smokeCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:demo','--screenshot',$smokeImage,'--profile',$testProfile) -LogDirectory $processLogs
if ($smokeCheck.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $smokeImage)) { throw ('The staged browser failed its startup check. ' + $smokeCheck.Stderr) }
$smoke = $smokeCheck.Stdout | ConvertFrom-Json
if ($smoke.version -ne $version -or $smoke.commands -lt 10) { throw 'The staged browser returned an unexpected startup result.' }
$scriptCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:javascript','--click-id','increment','--click-count','2') -LogDirectory $processLogs
if ($scriptCheck.ExitCode -ne 0) { throw ('The JavaScript check failed. ' + $scriptCheck.Stderr) }
$scriptSmoke = $scriptCheck.Stdout | ConvertFrom-Json
if ($scriptSmoke.title -ne 'Licznik: 2' -or $scriptSmoke.javascript.executed_scripts -ne 1 -or $scriptSmoke.javascript.diagnostics.Count -ne 0) { throw 'The DOM and JavaScript check returned an unexpected result.' }
$moduleTitle = 'Modu' + [char]0x0142 + 'y: 1'
$moduleCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:modules','--click-id','module-increment') -LogDirectory $processLogs
if ($moduleCheck.ExitCode -ne 0) { throw ('The JavaScript module check failed. ' + $moduleCheck.Stderr) }
$moduleSmoke = $moduleCheck.Stdout | ConvertFrom-Json
if ($moduleSmoke.title -ne $moduleTitle -or $moduleSmoke.javascript.executed_scripts -ne 2 -or $moduleSmoke.javascript.diagnostics.Count -ne 0) { throw 'The parser and JavaScript module check returned an unexpected result.' }
$asyncCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:async','--click-id','run-async','--run-for-ms','1600','--profile',$testProfile) -LogDirectory $processLogs
if ($asyncCheck.ExitCode -ne 0) { throw ('The asynchronous JavaScript check failed. ' + $asyncCheck.Stderr) }
$asyncSmoke = $asyncCheck.Stdout | ConvertFrom-Json
if ($asyncSmoke.title -ne 'Zadanie: OK' -or $asyncSmoke.javascript.executed_scripts -ne 2 -or $asyncSmoke.javascript.pending_requests -ne 0 -or $asyncSmoke.javascript.diagnostics.Count -ne 0) { throw 'The async/await, Fetch, and dynamic script check returned an unexpected result.' }
$routeDetails = 'Trasa: Szczeg' + [char]0x00F3 + [char]0x0142 + 'y'
$routeStart = 'Trasa: Pocz' + [char]0x0105 + 'tek'
$routeCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:routes','--click-id','route-details','--profile',$testProfile) -LogDirectory $processLogs
if ($routeCheck.ExitCode -ne 0) { throw ('The history check failed. ' + $routeCheck.Stderr) }
$routeSmoke = $routeCheck.Stdout | ConvertFrom-Json
if ($routeSmoke.title -ne $routeDetails -or $routeSmoke.url -ne 'about:routes#details' -or $routeSmoke.javascript.diagnostics.Count -ne 0) { throw 'The History, MutationObserver, Map, and RegExp check returned an unexpected result.' }
$bytesCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:bytes','--click-id','binary-run','--profile',$testProfile) -LogDirectory $processLogs
if ($bytesCheck.ExitCode -ne 0) { throw ('The binary data check failed. ' + $bytesCheck.Stderr) }
$bytesSmoke = $bytesCheck.Stdout | ConvertFrom-Json
if ($bytesSmoke.title -ne 'Dane binarne OK' -or $bytesSmoke.javascript.diagnostics.Count -ne 0) { throw 'The stream reading and multipart form check returned an unexpected result.' }
$timeDom = Join-Path $qaDirectory 'install-time-check.html'
$timeCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:time','--click-id','start-time','--run-for-ms','500','--dump-dom',$timeDom,'--profile',$testProfile) -LogDirectory $processLogs
if ($timeCheck.ExitCode -ne 0) { throw ('The clock check failed. ' + $timeCheck.Stderr) }
$timeSmoke = $timeCheck.Stdout | ConvertFrom-Json
if ($timeSmoke.title -ne 'Zegar aktywny' -or $timeSmoke.javascript.diagnostics.Count -ne 0 -or (Get-Content -LiteralPath $timeDom -Raw -Encoding UTF8) -notmatch 'data-sampled="true"') { throw 'The clock did not read the time and frame timestamp.' }
$canvasTitle = 'P' + [char]0x0142 + [char]0x00F3 + 'tno gotowe'
$canvasDom = Join-Path $qaDirectory 'install-canvas-check.html'
$canvasCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:canvas','--click-id','draw-canvas','--dump-dom',$canvasDom,'--profile',$testProfile) -LogDirectory $processLogs
if ($canvasCheck.ExitCode -ne 0) { throw ('The Canvas 2D check failed. ' + $canvasCheck.Stderr) }
$canvasSmoke = $canvasCheck.Stdout | ConvertFrom-Json
if ($canvasSmoke.title -ne $canvasTitle -or $canvasSmoke.javascript.diagnostics.Count -ne 0 -or (Get-Content -LiteralPath $canvasDom -Raw -Encoding UTF8) -notmatch 'RGBA\(24, 131, 111, 255\)') { throw 'Canvas 2D returned an unexpected pixel value.' }
$pipeDom = Join-Path $qaDirectory 'install-pipe-check.html'
$pipeCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:bytes','--click-id','binary-pipe','--run-for-ms','650','--dump-dom',$pipeDom,'--profile',$testProfile) -LogDirectory $processLogs
if ($pipeCheck.ExitCode -ne 0) { throw ('The stream pipeline check failed. ' + $pipeCheck.Stderr) }
$pipeSmoke = $pipeCheck.Stdout | ConvertFrom-Json
if ($pipeSmoke.title -ne 'Potok gotowy' -or $pipeSmoke.javascript.diagnostics.Count -ne 0 -or (Get-Content -LiteralPath $pipeDom -Raw -Encoding UTF8) -notmatch 'Odczyt BYOB: 8 B') { throw 'The stream pipeline failed to read, transfer, or transform data.' }
$numberDom = Join-Path $qaDirectory 'install-numbers-check.html'
$numberCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:numbers','--click-id','number-run','--dump-dom',$numberDom,'--profile',$testProfile) -LogDirectory $processLogs
if ($numberCheck.ExitCode -ne 0) { throw ('The large integer check failed. ' + $numberCheck.Stderr) }
$numberSmoke = $numberCheck.Stdout | ConvertFrom-Json
$numberText = Get-Content -LiteralPath $numberDom -Raw -Encoding UTF8
if ($numberSmoke.title -ne 'Liczby gotowe' -or $numberSmoke.javascript.diagnostics.Count -ne 0 -or $numberText -notmatch 'data-verified="yes"' -or $numberText -notmatch '340282366920938463426481119284349108225' -or $numberText -notmatch 'ff ff ff ff ff ff ff ff') { throw 'The BigInt, accessor, and 64-bit data check returned an unexpected result.' }
$messageTitle = 'Odebrano 42'
$messageDom = Join-Path $qaDirectory 'install-messages-check.html'
$messageCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:messages','--click-id','message-run','--run-for-ms','650','--dump-dom',$messageDom,'--profile',$testProfile) -LogDirectory $processLogs
if ($messageCheck.ExitCode -ne 0) { throw ('The message port check failed. ' + $messageCheck.Stderr) }
$messageSmoke = $messageCheck.Stdout | ConvertFrom-Json
if ($messageSmoke.title -ne $messageTitle -or $messageSmoke.javascript.diagnostics.Count -ne 0 -or (Get-Content -LiteralPath $messageDom -Raw -Encoding UTF8) -notmatch '>Odebrano 42\. Bufor') { throw 'The message ports did not return a response after transferring the buffer.' }
$fileTitle = 'Podgl' + [char]0x0105 + 'd gotowy'
$fileSample = Join-Path $processLogs 'probe-image.png'
[IO.File]::WriteAllBytes($fileSample, [Convert]::FromBase64String('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNg+M/wHwAEAQH/cetH5QAAAABJRU5ErkJggg=='))
$fileAnswers = Join-Path $processLogs 'file-picker-responses.json'
$fileSelections = ,([string[]]@($fileSample))
[IO.File]::WriteAllText($fileAnswers, (ConvertTo-Json -InputObject $fileSelections -Compress), [Text.UTF8Encoding]::new($false))
$fileDom = Join-Path $qaDirectory 'install-files-check.html'
$fileCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:files','--click-id','image-pick','--file-picker-responses',$fileAnswers,'--run-for-ms','3000','--dump-dom',$fileDom,'--profile',$testProfile) -LogDirectory $processLogs
if ($fileCheck.ExitCode -ne 0) { throw ('The file picker and image decoding check failed. ' + $fileCheck.Stderr) }
$fileSmoke = $fileCheck.Stdout | ConvertFrom-Json
$fileText = Get-Content -LiteralPath $fileDom -Raw -Encoding UTF8
if ($fileSmoke.title -ne $fileTitle -or $fileSmoke.javascript.diagnostics.Count -ne 0 -or $fileText -notmatch '>probe-image\.png' -or $fileText -notmatch '<canvas\b(?=[^>]*\bwidth="300")(?=[^>]*\bheight="300")[^>]*>') { throw 'The selected file did not appear on the canvas.' }
$graphicsCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--headless','--url','about:graphics','--click-id','move','--click-count','2') -LogDirectory $processLogs
if ($graphicsCheck.ExitCode -ne 0) { throw ('The graphics and measurements check failed. ' + $graphicsCheck.Stderr) }
$graphicsSmoke = $graphicsCheck.Stdout | ConvertFrom-Json
if ($graphicsSmoke.title -ne 'Grafika: 2' -or $graphicsSmoke.javascript.diagnostics.Count -ne 0 -or $graphicsSmoke.layout_limited -or $graphicsSmoke.svg.scenes -lt 1) { throw 'The CSSOM, SVG, and JavaScript syntax check returned an unexpected result.' }
$windowImage = Join-Path $qaDirectory 'install-window-check.png'
$windowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$windowImage,'--smoke-click-id','send','--url','about:forms','--profile',$testProfile) -LogDirectory $processLogs
if ($windowCheck.ExitCode -ne 0) { throw ('The staged browser failed its native window check. ' + $windowCheck.Stderr) }
$windowSmoke = $windowCheck.Stdout | ConvertFrom-Json
if (-not $windowSmoke.window_smoke -or $windowSmoke.frames_presented -lt 1 -or $windowSmoke.title -ne 'Formularz: OK') { throw 'The window did not present the form, Promise.all, and JSON result.' }
$graphicsImage = Join-Path $qaDirectory 'install-graphics-check.png'
$graphicsWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$graphicsImage,'--smoke-click-id','click-shape','--url','about:graphics','--profile',$testProfile) -LogDirectory $processLogs
if ($graphicsWindowCheck.ExitCode -ne 0) { throw ('The SVG click check failed. ' + $graphicsWindowCheck.Stderr) }
$graphicsWindowSmoke = $graphicsWindowCheck.Stdout | ConvertFrom-Json
if (-not $graphicsWindowSmoke.window_smoke -or $graphicsWindowSmoke.frames_presented -lt 1 -or $graphicsWindowSmoke.title -ne 'SVG: OK' -or $graphicsWindowSmoke.paint_limited -or $graphicsWindowSmoke.layout_limited -or $graphicsWindowSmoke.javascript.diagnostics.Count -ne 0) { throw 'The window did not present a frame after the SVG click.' }
$moduleImage = Join-Path $qaDirectory 'install-modules-check.png'
$moduleWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$moduleImage,'--smoke-click-id','module-increment','--url','about:modules','--profile',$testProfile) -LogDirectory $processLogs
if ($moduleWindowCheck.ExitCode -ne 0) { throw ('The JavaScript module check in the window failed. ' + $moduleWindowCheck.Stderr) }
$moduleWindowSmoke = $moduleWindowCheck.Stdout | ConvertFrom-Json
if (-not $moduleWindowSmoke.window_smoke -or $moduleWindowSmoke.frames_presented -lt 1 -or $moduleWindowSmoke.title -ne $moduleTitle -or $moduleWindowSmoke.javascript.diagnostics.Count -ne 0) { throw 'The window did not present a frame after the module ran and the page received the click.' }
$asyncImage = Join-Path $qaDirectory 'install-async-check.png'
$asyncActions = Join-Path $processLogs 'async-actions.json'
[IO.File]::WriteAllText($asyncActions, '[{"action":"click","id":"run-async"},{"action":"click","id":"cancel-async"}]', [Text.UTF8Encoding]::new($false))
$asyncWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$asyncImage,'--smoke-actions',$asyncActions,'--url','about:async','--profile',$testProfile) -LogDirectory $processLogs
if ($asyncWindowCheck.ExitCode -ne 0) { throw ('The cancellation check in the window failed. ' + $asyncWindowCheck.Stderr) }
$asyncWindowSmoke = $asyncWindowCheck.Stdout | ConvertFrom-Json
if (-not $asyncWindowSmoke.window_smoke -or $asyncWindowSmoke.frames_presented -lt 1 -or $asyncWindowSmoke.title -ne 'Zadanie: anulowano' -or $asyncWindowSmoke.javascript.diagnostics.Count -ne 0) { throw 'The window did not present the canceled task result.' }
$routeImage = Join-Path $qaDirectory 'install-routes-check.png'
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
if ($routeWindowCheck.ExitCode -ne 0) { throw ('The history navigation check in the window failed. ' + $routeWindowCheck.Stderr) }
$routeWindowSmoke = $routeWindowCheck.Stdout | ConvertFrom-Json
if (-not $routeWindowSmoke.window_smoke -or $routeWindowSmoke.frames_presented -lt 1 -or $routeWindowSmoke.url -ne 'about:routes#details' -or $routeWindowSmoke.javascript.diagnostics.Count -ne 0) { throw 'The window did not present a frame after back and forward navigation.' }
$bytesImage = Join-Path $qaDirectory 'install-bytes-check.png'
$bytesWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$bytesImage,'--smoke-click-id','binary-run','--url','about:bytes','--profile',$testProfile) -LogDirectory $processLogs
if ($bytesWindowCheck.ExitCode -ne 0) { throw ('The file and stream check in the window failed. ' + $bytesWindowCheck.Stderr) }
$bytesWindowSmoke = $bytesWindowCheck.Stdout | ConvertFrom-Json
if (-not $bytesWindowSmoke.window_smoke -or $bytesWindowSmoke.frames_presented -lt 1 -or $bytesWindowSmoke.title -ne 'Dane binarne OK' -or $bytesWindowSmoke.javascript.diagnostics.Count -ne 0 -or $bytesWindowSmoke.layout_limited -or $bytesWindowSmoke.paint_limited) { throw 'The window did not present the binary data and form result.' }
$timeImage = Join-Path $qaDirectory 'install-time-window.png'
$timeWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$timeImage,'--smoke-click-id','start-time','--url','about:time','--profile',$testProfile) -LogDirectory $processLogs
if ($timeWindowCheck.ExitCode -ne 0) { throw ('The clock check in the window failed. ' + $timeWindowCheck.Stderr) }
$timeWindowSmoke = $timeWindowCheck.Stdout | ConvertFrom-Json
if (-not $timeWindowSmoke.window_smoke -or $timeWindowSmoke.frames_presented -lt 1 -or $timeWindowSmoke.title -ne 'Zegar aktywny' -or $timeWindowSmoke.javascript.diagnostics.Count -ne 0 -or $timeWindowSmoke.layout_limited -or $timeWindowSmoke.paint_limited) { throw 'The window did not present the active clock.' }
$canvasImage = Join-Path $qaDirectory 'install-canvas-window.png'
$canvasWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$canvasImage,'--smoke-click-id','draw-canvas','--url','about:canvas','--profile',$testProfile) -LogDirectory $processLogs
if ($canvasWindowCheck.ExitCode -ne 0) { throw ('The canvas check in the window failed. ' + $canvasWindowCheck.Stderr) }
$canvasWindowSmoke = $canvasWindowCheck.Stdout | ConvertFrom-Json
if (-not $canvasWindowSmoke.window_smoke -or $canvasWindowSmoke.frames_presented -lt 1 -or $canvasWindowSmoke.title -ne $canvasTitle -or $canvasWindowSmoke.javascript.diagnostics.Count -ne 0 -or $canvasWindowSmoke.layout_limited -or $canvasWindowSmoke.paint_limited) { throw 'The window did not present the canvas drawing.' }
$pipeImage = Join-Path $qaDirectory 'install-pipe-window.png'
$pipeActions = Join-Path $processLogs 'pipe-actions.json'
[IO.File]::WriteAllText($pipeActions, '[{"action":"click","id":"binary-pipe"},{"action":"title","value":"Potok gotowy"}]', [Text.UTF8Encoding]::new($false))
$pipeWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$pipeImage,'--smoke-actions',$pipeActions,'--url','about:bytes','--profile',$testProfile) -LogDirectory $processLogs
if ($pipeWindowCheck.ExitCode -ne 0) { throw ('The stream pipeline check in the window failed. ' + $pipeWindowCheck.Stderr) }
$pipeWindowSmoke = $pipeWindowCheck.Stdout | ConvertFrom-Json
if (-not $pipeWindowSmoke.window_smoke -or $pipeWindowSmoke.frames_presented -lt 1 -or $pipeWindowSmoke.title -ne 'Potok gotowy' -or $pipeWindowSmoke.javascript.diagnostics.Count -ne 0) { throw 'The window did not present the stream pipeline result.' }
$numberImage = Join-Path $qaDirectory 'install-numbers-window.png'
$numberWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$numberImage,'--smoke-click-id','number-run','--url','about:numbers','--profile',$testProfile) -LogDirectory $processLogs
if ($numberWindowCheck.ExitCode -ne 0) { throw ('The large integer check in the window failed. ' + $numberWindowCheck.Stderr) }
$numberWindowSmoke = $numberWindowCheck.Stdout | ConvertFrom-Json
if (-not $numberWindowSmoke.window_smoke -or $numberWindowSmoke.frames_presented -lt 1 -or $numberWindowSmoke.title -ne 'Liczby gotowe' -or $numberWindowSmoke.javascript.diagnostics.Count -ne 0 -or $numberWindowSmoke.layout_limited -or $numberWindowSmoke.paint_limited) { throw 'The window did not present the BigInt calculation result.' }
$messageImage = Join-Path $qaDirectory 'install-messages-window.png'
$messageActions = Join-Path $processLogs 'messages-actions.json'
[IO.File]::WriteAllText($messageActions, '[{"action":"click","id":"message-run"},{"action":"title","value":"Odebrano 42"}]', [Text.UTF8Encoding]::new($false))
$messageWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$messageImage,'--smoke-actions',$messageActions,'--url','about:messages','--profile',$testProfile) -LogDirectory $processLogs
if ($messageWindowCheck.ExitCode -ne 0) { throw ('The message port check in the window failed. ' + $messageWindowCheck.Stderr) }
$messageWindowSmoke = $messageWindowCheck.Stdout | ConvertFrom-Json
if (-not $messageWindowSmoke.window_smoke -or $messageWindowSmoke.frames_presented -lt 1 -or $messageWindowSmoke.title -ne $messageTitle -or $messageWindowSmoke.javascript.diagnostics.Count -ne 0) { throw 'The window did not present the MessagePort response.' }
$fileImage = Join-Path $qaDirectory 'install-files-window.png'
$fileActions = Join-Path $processLogs 'files-actions.json'
$fileActionSteps = @(@{action='click';id='image-pick'}, @{action='wait_title';value=$fileTitle;timeout_ms=5000})
[IO.File]::WriteAllText($fileActions, (ConvertTo-Json -InputObject $fileActionSteps -Compress), [Text.UTF8Encoding]::new($false))
$fileWindowCheck = Invoke-BrowserProcess -Executable $candidate -Arguments @('--smoke-window',$fileImage,'--smoke-actions',$fileActions,'--url','about:files','--file-picker-responses',$fileAnswers,'--profile',$testProfile) -LogDirectory $processLogs
if ($fileWindowCheck.ExitCode -ne 0) { throw ('The file preview check in the window failed. ' + $fileWindowCheck.Stderr) }
$fileWindowSmoke = $fileWindowCheck.Stdout | ConvertFrom-Json
if (-not $fileWindowSmoke.window_smoke -or $fileWindowSmoke.frames_presented -lt 1 -or $fileWindowSmoke.title -ne $fileTitle -or $fileWindowSmoke.javascript.diagnostics.Count -ne 0) { throw 'The window did not present the selected file preview.' }
$mediaSmoke = Invoke-SatinMediaSmoke -Executable $candidate -ProfileDirectory $testProfile -OutputDirectory $qaDirectory -LogDirectory $processLogs

$manifest = [ordered]@{
    product = 'RustBrowser'
    version = $version
    buildId = $buildId
    executable = 'versions/' + $buildId + '/rust-browser.exe'
    sha256 = Get-SatinFileSha256 $candidate
    profileSchema = 1
}
$pendingLauncher = Join-Path $versionRoot 'launcher.pending.exe'
$pending = Join-Path $versionRoot 'current.pending'
$previous = Join-Path $installRoot 'previous.json'
Copy-Item -LiteralPath $launcher -Destination $pendingLauncher
[IO.File]::WriteAllText($pending, ($manifest | ConvertTo-Json), [Text.UTF8Encoding]::new($false))

$updateLock = Enter-SatinPublicationLock -InstallRoot $installRoot
try {
    $hasCurrent = Test-Path -LiteralPath $current
    if ($hasCurrent) {
        $null = ConvertFrom-SatinManifest ([IO.File]::ReadAllText($current))
    }
    foreach ($scriptName in @('Process.ps1', 'Rollback.ps1')) {
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot $scriptName) -Destination (Join-Path $installRoot $scriptName) -Force
    }
    if (Test-Path -LiteralPath $installedLauncher) {
        [IO.File]::Replace($pendingLauncher, $installedLauncher, (Join-Path $installRoot 'RustBrowser.previous.exe'))
    } else {
        [IO.File]::Move($pendingLauncher, $installedLauncher)
    }
    if ($hasCurrent) {
        [IO.File]::Replace($pending, $current, $previous)
    } else {
        [IO.File]::Move($pending, $current)
    }
} finally { $updateLock.Dispose() }

$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $installedLauncher
$shortcut.WorkingDirectory = $installRoot
$shortcut.IconLocation = $installedLauncher + ',0'
$shortcut.Description = 'Satin web browser'
$shortcut.Save()
[ordered]@{
    version=$version; buildId=$buildId; shortcut=$shortcutPath; installation=$installRoot; receipts=$qaDirectory
    smoke=$smoke; javascript=$scriptSmoke; graphics=$graphicsSmoke; windowSmoke=$windowSmoke
    graphicsWindowSmoke=$graphicsWindowSmoke; modules=$moduleSmoke; moduleWindowSmoke=$moduleWindowSmoke
    async=$asyncSmoke; asyncWindowSmoke=$asyncWindowSmoke; routes=$routeSmoke; routeWindowSmoke=$routeWindowSmoke
    bytes=$bytesSmoke; bytesWindowSmoke=$bytesWindowSmoke; time=$timeSmoke; timeWindowSmoke=$timeWindowSmoke
    canvas=$canvasSmoke; canvasWindowSmoke=$canvasWindowSmoke; pipe=$pipeSmoke; pipeWindowSmoke=$pipeWindowSmoke
    numbers=$numberSmoke; numbersWindowSmoke=$numberWindowSmoke; messages=$messageSmoke
    messagesWindowSmoke=$messageWindowSmoke; files=$fileSmoke; filesWindowSmoke=$fileWindowSmoke; media=$mediaSmoke
} | ConvertTo-Json -Depth 10
