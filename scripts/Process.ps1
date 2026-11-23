function Invoke-BrowserProcess {
    param(
        [Parameter(Mandatory=$true)][string]$Executable,
        [string[]]$Arguments = @(),
        [Parameter(Mandatory=$true)][string]$LogDirectory,
        [int]$TimeoutMilliseconds = 45000
    )
    $LogDirectory = (New-Item -ItemType Directory -Path $LogDirectory -Force).FullName
    $token = [Guid]::NewGuid().ToString('N')
    $stdout = Join-Path $LogDirectory ($token + '.out')
    $stderr = Join-Path $LogDirectory ($token + '.err')
    # Quote each argument using Windows rules, including trailing slashes
    # and embedded quotes. ArgumentList is unavailable on Windows PowerShell 5.1.
    $quoted = @($Arguments | ForEach-Object {
        $escaped = [regex]::Replace($_, '(\\*)"', '$1$1\"')
        $escaped = [regex]::Replace($escaped, '(\\+)$', '$1$1')
        '"' + $escaped + '"'
    })
    $utf8 = [Text.UTF8Encoding]::new($false)
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $Executable
    $start.Arguments = $quoted -join ' '
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = $utf8
    $start.StandardErrorEncoding = $utf8
    $child = [Diagnostics.Process]::Start($start)
    try {
        $output = $child.StandardOutput.ReadToEndAsync()
        $errors = $child.StandardError.ReadToEndAsync()
        $finished = $child.WaitForExit($TimeoutMilliseconds)
        if (-not $finished) {
            $child.Kill()
            $child.WaitForExit()
        }
        $outputText = $output.GetAwaiter().GetResult()
        $errorText = $errors.GetAwaiter().GetResult()
        [IO.File]::WriteAllText($stdout, $outputText, $utf8)
        [IO.File]::WriteAllText($stderr, $errorText, $utf8)
        if (-not $finished) { throw "Test process $($child.Id) timed out. See $stderr for its log." }
        [pscustomobject]@{
            Id = $child.Id
            ExitCode = $child.ExitCode
            Stdout = $outputText
            Stderr = $errorText
            StdoutPath = $stdout
            StderrPath = $stderr
        }
    } finally { $child.Dispose() }
}

function Enter-SatinPublicationLock {
    param([Parameter(Mandatory=$true)][string]$InstallRoot)

    $path = Join-Path $InstallRoot 'update.lock'
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        try {
            # Share mode zero matches the Windows lock used by the browser updater.
            return [IO.File]::Open($path, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        } catch [IO.IOException] {
            $code = $_.Exception.GetBaseException().HResult -band 0xffff
            if ($code -notin 32, 33) { throw }
            if ($timer.ElapsedMilliseconds -ge 5000) {
                throw 'Another Satin process is updating the installation. Try again shortly.'
            }
            Start-Sleep -Milliseconds 250
        }
    }
}

function ConvertFrom-SatinManifest {
    param([Parameter(Mandatory=$true)][string]$Json)

    $manifest = ConvertFrom-Json -InputObject $Json
    if ($manifest.product -cne 'RustBrowser' -or $manifest.profileSchema -ne 1) {
        throw 'The existing installation has an unsupported product or profile schema.'
    }
    $manifest
}

function Invoke-SatinMediaSmoke {
    param(
        [Parameter(Mandatory=$true)][string]$Executable,
        [Parameter(Mandatory=$true)][string]$ProfileDirectory,
        [Parameter(Mandatory=$true)][string]$OutputDirectory,
        [Parameter(Mandatory=$true)][string]$LogDirectory
    )

    $title = 'Satin MP4 OK'
    $dom = Join-Path $OutputDirectory 'media.html'
    $image = Join-Path $OutputDirectory 'media.png'
    $headless = Invoke-BrowserProcess -Executable $Executable -LogDirectory $LogDirectory -TimeoutMilliseconds 15000 -Arguments @(
        '--headless', '--url', 'about:media', '--click-id', 'media-play',
        '--run-for-ms', '3000', '--dump-dom', $dom, '--screenshot', $image, '--profile', $ProfileDirectory
    )
    if ($headless.ExitCode -ne 0) { throw "The headless MP4 check failed. See $($headless.StderrPath)." }
    $headlessResult = $headless.Stdout | ConvertFrom-Json
    $html = Get-Content -LiteralPath $dom -Raw -Encoding UTF8
    if ($headlessResult.title -cne $title -or $headlessResult.javascript.diagnostics.Count -ne 0 -or
        $html -notmatch '<p\b(?=[^>]*\bid="media-status")(?=[^>]*\bdata-progress="yes")[^>]*>' -or
        -not (Test-Path -LiteralPath $image -PathType Leaf)) {
        throw "The headless MP4 check did not record playback progress. See $dom and $($headless.StdoutPath)."
    }

    $windowImage = Join-Path $OutputDirectory 'media-window.png'
    $actions = Join-Path $LogDirectory 'media-actions.json'
    $steps = @(
        @{action='click'; id='media-play'},
        @{action='wait_title'; value=$title; timeout_ms=8000}
    )
    [IO.File]::WriteAllText($actions, (ConvertTo-Json -InputObject $steps -Compress), [Text.UTF8Encoding]::new($false))
    $native = Invoke-BrowserProcess -Executable $Executable -LogDirectory $LogDirectory -TimeoutMilliseconds 20000 -Arguments @(
        '--smoke-window', $windowImage, '--smoke-actions', $actions, '--url', 'about:media', '--profile', $ProfileDirectory
    )
    if ($native.ExitCode -ne 0) { throw "The native MP4 check failed. See $($native.StderrPath)." }
    $nativeResult = $native.Stdout | ConvertFrom-Json
    if (-not $nativeResult.window_smoke -or $nativeResult.frames_presented -lt 1 -or
        $nativeResult.title -cne $title -or $nativeResult.javascript.diagnostics.Count -ne 0 -or
        $nativeResult.layout_limited -or $nativeResult.paint_limited -or -not (Test-Path -LiteralPath $windowImage -PathType Leaf)) {
        throw "The native MP4 check did not present a playback frame. See $($native.StdoutPath)."
    }

    [pscustomobject]@{
        headless = $headlessResult
        native = $nativeResult
        dom = $dom
        screenshot = $image
        nativeScreenshot = $windowImage
        headlessLog = $headless.StdoutPath
        headlessErrorLog = $headless.StderrPath
        nativeLog = $native.StdoutPath
        nativeErrorLog = $native.StderrPath
        actions = $actions
    }
}
