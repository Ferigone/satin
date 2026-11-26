param(
    [string]$ReleaseDirectory,
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Process.ps1')
$projectDirectory = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if (-not $ReleaseDirectory) { $ReleaseDirectory = Join-Path $projectDirectory 'target\release' }
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $projectDirectory 'dist\windows-x64' }

$browser = Get-Item -LiteralPath (Join-Path $ReleaseDirectory 'rust-browser.exe')
$launcher = Get-Item -LiteralPath (Join-Path $ReleaseDirectory 'rust-browser-launcher.exe')
$OutputDirectory = (New-Item -ItemType Directory -Path $OutputDirectory -Force).FullName
$updateAsset = Join-Path $OutputDirectory 'satin-windows-x64-opt-in.exe'
$installAsset = Join-Path $OutputDirectory 'Satin-windows-x64.zip'
if (Test-Path -LiteralPath (Join-Path $OutputDirectory 'satin-windows-x64.exe')) {
    throw 'The output directory contains a legacy auto-update asset. Use a clean directory.'
}
Copy-Item -LiteralPath $browser.FullName -Destination $updateAsset -Force

Add-Type -AssemblyName System.IO.Compression
$utf8 = [Text.UTF8Encoding]::new($false)
# Windows PowerShell uses a legacy code page for scripts without a BOM.
$scriptEncoding = [Text.UTF8Encoding]::new($true)
$archiveTime = [DateTimeOffset]'2000-01-01T00:00:00+00:00'
function New-ZipEntry([string]$Name) {
    $entry = $archive.CreateEntry($Name)
    # Stable metadata lets a rebuilt archive reuse matching assets in a draft.
    $entry.LastWriteTime = $archiveTime
    $entry.Open()
}
function Write-ZipFile([string]$Name, [string]$Path) {
    $source = [IO.File]::OpenRead($Path)
    try {
        $stream = New-ZipEntry $Name
        try { $source.CopyTo($stream) } finally { $stream.Dispose() }
    } finally { $source.Dispose() }
}
function Write-ZipText([string]$Name, [string]$Contents, [Text.Encoding]$Encoding) {
    $writer = [IO.StreamWriter]::new((New-ZipEntry $Name), $Encoding)
    try { $writer.Write($Contents) } finally { $writer.Dispose() }
}

$installCommand = @'
@echo off
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\Install.ps1" -SkipBuild
set "result=%errorlevel%"
if not "%result%"=="0" pause
exit /b %result%
'@
$instructions = @'
Satin for Windows x64

Extract the archive to a folder and double-click Install.cmd.
After setup, use the Satin desktop shortcut to launch the browser.
Use this archive to upgrade from alpha.44 or older; those builds will
not automatically download this release.
'@

$zipStream = [IO.File]::Open([IO.Path]::GetFullPath($installAsset), [IO.FileMode]::Create)
try {
    $archive = [IO.Compression.ZipArchive]::new($zipStream, [IO.Compression.ZipArchiveMode]::Create, $true)
    try {
        foreach ($binary in @($browser, $launcher)) {
            Write-ZipFile "target/release/$($binary.Name)" $binary.FullName
        }
        foreach ($scriptName in @('Install.ps1', 'Process.ps1', 'Rollback.ps1')) {
            $source = Join-Path $PSScriptRoot $scriptName
            Write-ZipText "scripts/$scriptName" ([IO.File]::ReadAllText($source)) $scriptEncoding
        }
        foreach ($license in (Get-ChildItem -LiteralPath $projectDirectory -File -Filter 'LICENSE*' | Sort-Object Name)) {
            Write-ZipFile $license.Name $license.FullName
        }
        Write-ZipText 'Install.cmd' (($installCommand -replace '\r?\n', "`r`n") + "`r`n") $utf8
        Write-ZipText 'README.txt' ($instructions + "`r`n") $utf8
    } finally { $archive.Dispose() }
} finally { $zipStream.Dispose() }

$checksums = foreach ($asset in @($updateAsset, $installAsset)) {
    '{0}  {1}' -f (Get-SatinFileSha256 $asset).ToLowerInvariant(), [IO.Path]::GetFileName($asset)
}
[IO.File]::WriteAllLines([IO.Path]::GetFullPath((Join-Path $OutputDirectory 'SHA256SUMS.txt')), $checksums, $utf8)
