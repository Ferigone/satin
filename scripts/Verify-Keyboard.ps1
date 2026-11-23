param([string]$Executable)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Process.ps1')
$project = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if (-not $Executable) { $Executable = Join-Path $project 'target\release\rust-browser.exe' }
$binary = (Resolve-Path -LiteralPath $Executable).Path
$fixtures = Join-Path $project 'tools\fixtures\keyboard'
$run = Join-Path $project ('target\keyboard-verification\' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $run -Force | Out-Null

$report = [ordered]@{ executable = $binary; cases = [ordered]@{} }
foreach ($name in @('controls', 'link')) {
    $screenshot = Join-Path $run "$name.png"
    $case = [ordered]@{ passed = $false }
    $report.cases[$name] = $case
    try {
        $page = [Uri]::new((Join-Path $fixtures "$name.html"))
        $native = Invoke-BrowserProcess -Executable $binary -LogDirectory (Join-Path $run 'logs') -Arguments @(
            '--smoke-window', $screenshot, '--smoke-actions', (Join-Path $fixtures "$name.json"),
            '--url', $page.AbsoluteUri, '--profile', (Join-Path $run "$name-profile")
        )
        $case.stdout = $native.StdoutPath
        $case.stderr = $native.StderrPath
        if ($native.ExitCode -ne 0) { throw "Exit $($native.ExitCode): $($native.Stderr.Trim())" }
        $case.window = $native.Stdout | ConvertFrom-Json
        if (-not $case.window.window_smoke -or $case.window.frames_presented -lt 1 -or
            $case.window.javascript.diagnostics.Count -ne 0 -or -not (Test-Path -LiteralPath $screenshot)) {
            throw 'Expected a native frame, a screenshot, and no JavaScript diagnostics.'
        }
        $expectedUrl = if ($name -eq 'link') {
            [Uri]::new($page, 'destination.html').AbsoluteUri
        } else { $page.AbsoluteUri }
        if ($case.window.url -cne $expectedUrl) {
            throw "Expected final URL '$expectedUrl', got '$($case.window.url)'."
        }
        $case.screenshot = $screenshot
        $case.passed = $true
    } catch {
        $case.error = $_.Exception.Message
    }
}

$reportFile = Join-Path $run 'report.json'
[IO.File]::WriteAllText($reportFile, ($report | ConvertTo-Json -Depth 12), [Text.UTF8Encoding]::new($false))
foreach ($name in $report.cases.Keys) {
    $case = $report.cases[$name]
    if ($case.passed) { Write-Output "PASS ${name}: $($case.window.title)" }
    else { Write-Output "FAIL ${name}: $($case.error)" }
}
Write-Output "Report: $reportFile"
if (@($report.cases.Values | Where-Object { -not $_.passed }).Count -ne 0) { exit 1 }
