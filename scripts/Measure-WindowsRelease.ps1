param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$Sha256,
    [ValidateRange(3, 25)][int]$Runs = 5
)

$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
. (Join-Path $PSScriptRoot 'Process.ps1')

if (-not ('SatinMeasurement.Window' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
namespace SatinMeasurement {
    public static class Window {
        [StructLayout(LayoutKind.Sequential)]
        private struct Rect { public int Left, Top, Right, Bottom; }
        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool GetClientRect(IntPtr window, out Rect rect);
        [DllImport("user32.dll")]
        private static extern uint GetDpiForWindow(IntPtr window);
        public static int[] Geometry(IntPtr window) {
            Rect rect;
            if (!GetClientRect(window, out rect)) throw new Win32Exception();
            uint dpi = GetDpiForWindow(window);
            if (dpi == 0) throw new InvalidOperationException("Window DPI unavailable.");
            return new int[] { rect.Right - rect.Left, rect.Bottom - rect.Top, (int)dpi };
        }
    }
}
'@
}

function Get-Median([double[]]$Values) {
    $sorted = @($Values | Sort-Object)
    $middle = [int][Math]::Floor($sorted.Count / 2)
    if ($sorted.Count % 2 -eq 1) { return $sorted[$middle] }
    return ($sorted[$middle - 1] + $sorted[$middle]) / 2
}

function Measure-Launch([int]$Index) {
    $run = [IO.Directory]::CreateDirectory((Join-Path $receiptPath ("run-$Index"))).FullName
    $profile = Join-Path $run 'User Data'
    $appData = [IO.Directory]::CreateDirectory((Join-Path $run 'LocalAppData')).FullName
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $staged
    $start.WorkingDirectory = $receiptPath
    $start.Arguments = '--url "{0}" --profile "{1}"' -f $fixtureUrl, $profile
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.EnvironmentVariables['LOCALAPPDATA'] = $appData
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $child = [Diagnostics.Process]::Start($start)
    try {
        $output = $child.StandardOutput.ReadToEndAsync()
        $errors = $child.StandardError.ReadToEndAsync()
        $firstWindow = $null
        $titleTime = $null
        while ($clock.ElapsedMilliseconds -lt 15000) {
            $child.Refresh()
            if ($child.HasExited) { throw "Browser exited before the readiness marker. See $run." }
            $window = $child.MainWindowHandle
            if ($window -ne [IntPtr]::Zero) {
                if ($null -eq $firstWindow) { $firstWindow = $clock.Elapsed.TotalMilliseconds }
                if ($child.MainWindowTitle.StartsWith('Satin Windows probe ready!', [StringComparison]::Ordinal)) {
                    $titleTime = $clock.Elapsed.TotalMilliseconds
                    break
                }
            }
            Start-Sleep -Milliseconds 10
        }
        if ($null -eq $titleTime) { throw "No readiness marker in 15 seconds. See $run." }
        $geometry = [SatinMeasurement.Window]::Geometry($window)
        $idle = [Diagnostics.Stopwatch]::StartNew()
        $samples = @(for ($offset = 5000; $offset -lt 10000; $offset += 250) {
            while ($idle.ElapsedMilliseconds -lt $offset) {
                Start-Sleep -Milliseconds ([int]($offset - $idle.ElapsedMilliseconds + 1))
            }
            $child.Refresh()
            if ($child.HasExited) { throw "Browser exited during sampling. See $run." }
            [ordered]@{
                targetMs = $offset
                observedMs = $idle.ElapsedMilliseconds
                workingSetBytes = $child.WorkingSet64
                privateBytes = $child.PrivateMemorySize64
            }
        })
        if (([SatinMeasurement.Window]::Geometry($window) -join ',') -ne ($geometry -join ',')) {
            throw "Window size or DPI changed during collection. See $run."
        }
        [ordered]@{
            run = $Index
            firstWindowMs = [Math]::Round($firstWindow, 2)
            pageTitleMs = [Math]::Round($titleTime, 2)
            widthPx = $geometry[0]; heightPx = $geometry[1]; dpi = $geometry[2]
            workingSetBytes = Get-Median @($samples | ForEach-Object { $_.workingSetBytes })
            privateBytes = Get-Median @($samples | ForEach-Object { $_.privateBytes })
            samples = $samples
        }
    } finally {
        try {
            if (-not $child.HasExited -and -not ($child.CloseMainWindow() -and $child.WaitForExit(3000))) {
                if (-not $child.HasExited) {
                    $child.Kill()
                    if (-not $child.WaitForExit(3000)) { throw "Tracked browser did not exit. See $run." }
                }
            }
            [IO.File]::WriteAllText((Join-Path $run 'stdout.log'), $output.GetAwaiter().GetResult())
            [IO.File]::WriteAllText((Join-Path $run 'stderr.log'), $errors.GetAwaiter().GetResult())
        } finally { $child.Dispose() }
    }
}

$receiptPath = [IO.Directory]::CreateDirectory((Join-Path $root ('target\windows-measurements\' + [Guid]::NewGuid().ToString('N')))).FullName
$staged = Join-Path $receiptPath 'rust-browser.exe'
[IO.File]::Copy((Get-Item -LiteralPath $Executable).FullName, $staged)
$digest = Get-SatinFileSha256 $staged
if ($digest -ine $Sha256) { throw "Staged binary SHA-256 does not match the expected digest. See $receiptPath." }
$versionCheck = Invoke-BrowserProcess -Executable $staged -Arguments @('--version') -LogDirectory $receiptPath
if ($versionCheck.ExitCode -ne 0 -or $versionCheck.Stdout.Trim() -cne "Satin $Version") {
    throw "Staged binary does not report Satin $Version. See $receiptPath."
}

$fixture = Join-Path $receiptPath 'offline-cards.html'
$fixtureUrl = ([Uri]$fixture).AbsoluteUri
$cards = @(for ($i = 1; $i -le 240; $i++) {
    "<article><h2>Card $i</h2><p>Status: available</p><p>Local layout and text sample.</p></article>"
})
$html = @'
<!doctype html><meta charset="utf-8"><title>Satin Windows probe loading</title>
<style>
body{margin:20px;background:#f4f5f7;font:16px sans-serif;color:#18334e}
main{display:grid;grid-template-columns:repeat(4,1fr);gap:10px}
article{padding:12px;background:#fff;border:1px solid #ccd1d8}
h2{font-size:19px}p{margin:4px 0}
</style>
<h1>Satin offline Windows measurement</h1><main id="cards">
@@CARDS@@
</main><script>
requestAnimationFrame(function () {
    var cards = document.getElementById('cards');
    if (cards.children.length === 240 && cards.getBoundingClientRect().height > 100)
        document.title = 'Satin Windows probe ready!';
});
</script>
'@
$fixtureHtml = ($html -replace "`r`n", "`n").Replace('@@CARDS@@', ($cards -join "`n"))
[IO.File]::WriteAllText($fixture, $fixtureHtml, [Text.UTF8Encoding]::new($false))

Write-Host "Measuring $($versionCheck.Stdout.Trim()); receipts: $receiptPath"
$started = [DateTime]::UtcNow
$warmup = Measure-Launch 0
$results = @(for ($run = 1; $run -le $Runs; $run++) { Measure-Launch $run })
$geometry = "$($warmup.widthPx)x$($warmup.heightPx)@dpi$($warmup.dpi)"
foreach ($result in $results) {
    if ("$($result.widthPx)x$($result.heightPx)@dpi$($result.dpi)" -cne $geometry) {
        throw "Window geometry changed between runs. Repeat on one monitor. See $receiptPath."
    }
}
$summary = [ordered]@{
    firstWindowMs = Get-Median @($results | ForEach-Object { $_.firstWindowMs })
    pageTitleMs = Get-Median @($results | ForEach-Object { $_.pageTitleMs })
    workingSetMiB = [Math]::Round((Get-Median @($results | ForEach-Object { $_.workingSetBytes })) / 1MB, 2)
    privateMiB = [Math]::Round((Get-Median @($results | ForEach-Object { $_.privateBytes })) / 1MB, 2)
}
[ordered]@{
    path = $receiptPath
    startedUtc = $started.ToString('o')
    finishedUtc = [DateTime]::UtcNow.ToString('o')
    windowsBuild = [Environment]::OSVersion.Version.ToString()
    cpu = (Get-CimInstance Win32_Processor | Select-Object -ExpandProperty Name) -join '; '
    version = $Version
    sha256 = $digest
    executableBytes = (Get-Item -LiteralPath $staged).Length
    fixtureSha256 = Get-SatinFileSha256 $fixture
    geometry = $geometry
    summary = $summary
    warmup = $warmup
    runs = $results
} | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $receiptPath 'measurement.json') -Encoding UTF8
$summary
