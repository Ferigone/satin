$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Process.ps1')
$installRoot = Join-Path $env:LOCALAPPDATA 'RustBrowser'
$current = Join-Path $installRoot 'current.json'
$previous = Join-Path $installRoot 'previous.json'
if (-not (Test-Path -LiteralPath $previous -PathType Leaf)) { throw 'There is no previous Satin version to restore.' }
$previousJson = [IO.File]::ReadAllText($previous)
$metadata = ConvertFrom-SatinManifest $previousJson
if ($metadata.buildId -cnotmatch '^[A-Za-z0-9.+-]{1,160}$' -or $metadata.buildId -in '.', '..' -or
    $metadata.executable -cne "versions/$($metadata.buildId)/rust-browser.exe") {
    throw 'The previous Satin version has an invalid executable path.'
}
$target = Join-Path $installRoot $metadata.executable
if (-not (Test-Path -LiteralPath $target -PathType Leaf) -or (Get-SatinFileSha256 $target) -ne $metadata.sha256) {
    throw 'The previous Satin executable is missing or has changed.'
}

$pending = Join-Path $installRoot ('rollback-' + [Guid]::NewGuid().ToString('N') + '.pending')
[IO.File]::WriteAllText($pending, $previousJson, [Text.UTF8Encoding]::new($false))
try {
    $updateLock = Enter-SatinPublicationLock -InstallRoot $installRoot
    try {
        $null = ConvertFrom-SatinManifest ([IO.File]::ReadAllText($current))
        if ([IO.File]::ReadAllText($previous) -cne $previousJson) {
            throw 'The previous Satin version changed during verification. Run rollback again.'
        }
        [IO.File]::Replace($pending, $current, $previous)
    } finally { $updateLock.Dispose() }
} finally {
    if ([IO.File]::Exists($pending)) { [IO.File]::Delete($pending) }
}
Write-Output "Restored Satin $($metadata.version). Close and reopen the browser to use it."
